package cc.wckd.omnid

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.view.Gravity
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.view.WindowInsets
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.TextView
import android.widget.Toast
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/**
 * The daemon's own search page in a WebView. Only the daemon's origin loads inside; clearnet
 * links and magnets go to the apps that handle them.
 */
class MainActivity : Activity() {
    private lateinit var web: WebView
    private lateinit var status: TextView
    private var loaded: OmnidService.Endpoint? = null
    /** An address shared to the app before the daemon was ready. */
    private var pendingShare: String? = null
    private val onServiceChange: () -> Unit = { runOnUiThread(::show) }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        web = WebView(this).apply {
            settings.javaScriptEnabled = true
            settings.allowFileAccess = false
            settings.allowContentAccess = false
            settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            setBackgroundColor(Color.TRANSPARENT)
            webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    if (request.url.host == "127.0.0.1") return false
                    openElsewhere(request.url)
                    return true
                }
            }
            visibility = View.INVISIBLE
        }
        WebView.setWebContentsDebuggingEnabled(applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0)
        status = TextView(this).apply {
            gravity = Gravity.CENTER
            setPadding(48, 48, 48, 48)
            text = getString(R.string.starting)
        }
        val root = FrameLayout(this).apply {
            addView(web, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
            addView(status, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
            // Edge to edge from Android 15: keep the page clear of the system bars.
            setOnApplyWindowInsetsListener { view, insets ->
                val bars = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime())
                } else {
                    @Suppress("DEPRECATION")
                    insets.systemWindowInsets
                }
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                insets
            }
        }
        setContentView(root)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
        startForegroundService(Intent(this, OmnidService::class.java))
        takeShare(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        takeShare(intent)
    }

    override fun onStart() {
        super.onStart()
        OmnidService.observe(onServiceChange)
        show()
    }

    override fun onStop() {
        OmnidService.unobserve(onServiceChange)
        super.onStop()
    }

    @Deprecated("Platform back handling; there is no AndroidX here")
    override fun onBackPressed() {
        if (web.canGoBack()) web.goBack() else super.onBackPressed()
    }

    override fun onCreateOptionsMenu(menu: Menu): Boolean {
        menu.add(Menu.NONE, R.string.crawl_anytime, Menu.NONE, R.string.crawl_anytime).isCheckable = true
        menu.add(Menu.NONE, R.string.stop_omnid, Menu.NONE, R.string.stop_omnid)
        return true
    }

    override fun onPrepareOptionsMenu(menu: Menu): Boolean {
        menu.findItem(R.string.crawl_anytime).isChecked = prefs().getBoolean(CrawlPolicy.KEY_ANYTIME, false)
        return true
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean = when (item.itemId) {
        R.string.crawl_anytime -> {
            prefs().edit().putBoolean(CrawlPolicy.KEY_ANYTIME, !item.isChecked).apply()
            true
        }
        R.string.stop_omnid -> {
            stopService(Intent(this, OmnidService::class.java))
            finishAndRemoveTask()
            true
        }
        else -> super.onOptionsItemSelected(item)
    }

    private fun prefs() = getSharedPreferences(CrawlPolicy.PREFS, MODE_PRIVATE)

    private fun show() {
        val endpoint = OmnidService.endpoint
        val failure = OmnidService.failure
        when {
            endpoint != null -> {
                if (endpoint != loaded) {
                    // The page's own requests carry the token as a cookie.
                    CookieManager.getInstance()
                        .setCookie(endpoint.origin, "omnid_token=${endpoint.token}; HttpOnly; SameSite=Strict")
                    web.loadUrl("${endpoint.origin}/?embedded")
                    loaded = endpoint
                }
                web.visibility = View.VISIBLE
                status.visibility = View.GONE
                pendingShare?.let { submit(endpoint, it) }
                pendingShare = null
            }
            failure != null -> {
                status.text = getString(R.string.failed, failure)
                status.visibility = View.VISIBLE
            }
            else -> {
                status.text = getString(R.string.starting)
                status.visibility = View.VISIBLE
            }
        }
    }

    private fun takeShare(intent: Intent?) {
        if (intent?.action != Intent.ACTION_SEND) return
        val text = intent.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        val address = Regex("""magnet:\?\S+|https?://\S+|\S+\.(onion|i2p)\S*|(freenet|hyphanet):\S+""")
            .find(text)?.value
        if (address == null) {
            Toast.makeText(this, R.string.nothing_to_queue, Toast.LENGTH_LONG).show()
            return
        }
        val endpoint = OmnidService.endpoint
        if (endpoint == null) pendingShare = address else submit(endpoint, address)
    }

    /** POST /api/submit, off the main thread. */
    private fun submit(endpoint: OmnidService.Endpoint, address: String) {
        Thread {
            val message = runCatching {
                val connection = URL("${endpoint.origin}/api/submit").openConnection() as HttpURLConnection
                try {
                    connection.requestMethod = "POST"
                    connection.doOutput = true
                    connection.setRequestProperty("Content-Type", "application/json")
                    connection.setRequestProperty("Authorization", "Bearer ${endpoint.token}")
                    connection.outputStream.use { it.write(JSONObject().put("url", address).toString().toByteArray()) }
                    val ok = connection.responseCode in 200..299
                    val body = (if (ok) connection.inputStream else connection.errorStream).bufferedReader().use { it.readText() }
                    val json = JSONObject(body)
                    if (ok) getString(R.string.queued, json.getString("accepted")) else json.getString("error")
                } finally {
                    connection.disconnect()
                }
            }.getOrElse { it.message ?: it.toString() }
            runOnUiThread { Toast.makeText(this, message, Toast.LENGTH_LONG).show() }
        }.start()
    }

    private fun openElsewhere(uri: Uri) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, uri.toString(), Toast.LENGTH_LONG).show()
        }
    }
}
