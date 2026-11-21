package cc.wckd.omnid

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.ActivityOptions
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
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import android.content.res.ColorStateList
import android.util.TypedValue
import android.widget.Button
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import java.net.HttpURLConnection
import java.net.URL
import org.json.JSONObject

/**
 * The daemon's own search page in a WebView. Only the daemon's origin loads inside; web results
 * open in omnid's browser and magnets in the app that handles them.
 */
class MainActivity : Activity() {
    private companion object {
        const val KEY_PAGE = "page"
        /** The page's link to the i2pd app (app.js): opens it, or its F-Droid page. */
        const val I2PD_LINK = "omnid:i2pd"
    }

    private lateinit var web: WebView
    /** The start-up screen: shown until the daemon answers, or with the reason it failed. */
    private lateinit var starting: LinearLayout
    private lateinit var startingTitle: TextView
    private lateinit var startingDetail: TextView
    private lateinit var startingProgress: ProgressBar
    private lateinit var retry: Button
    private var loaded: OmnidService.Endpoint? = null
    /** An address shared to the app before the daemon was ready. */
    private var pendingShare: String? = null
    /** The page as it was before the activity was recreated (a theme switch), search included. */
    private var restorePage: String? = null
    /** System Back goes back in the page first; registered only while it can (Android 13+). */
    private var backCallback: Any? = null
    private val onServiceChange: () -> Unit = { runOnUiThread(::show) }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        restorePage = savedInstanceState?.getString(KEY_PAGE)
        web = WebView(this).apply {
            // The page draws its own; the platform's grey bar would sit on top of it.
            isVerticalScrollBarEnabled = false
            settings.javaScriptEnabled = true
            settings.allowFileAccess = false
            settings.allowContentAccess = false
            settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            setBackgroundColor(Color.TRANSPARENT)
            webViewClient = object : WebViewClient() {
                override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                    val uri = request.url
                    if (uri.host == "127.0.0.1") return false
                    if (uri.toString() == I2PD_LINK) {
                        launch(I2pd.intent(this@MainActivity))
                    } else if (BrowserConfig.isWebPage(uri.scheme, uri.host)) {
                        val open = ActivityOptions.makeCustomAnimation(this@MainActivity, R.anim.browser_open_enter, R.anim.browser_open_exit)
                        startActivity(
                            Intent(this@MainActivity, BrowserActivity::class.java).putExtra(BrowserActivity.EXTRA_URL, uri.toString()),
                            open.toBundle(),
                        )
                    } else {
                        openElsewhere(uri)
                    }
                    return true
                }

                override fun doUpdateVisitedHistory(view: WebView, url: String?, isReload: Boolean) = updateBack()

                override fun onPageFinished(view: WebView, url: String?) = tellPage()
            }
            visibility = View.INVISIBLE
        }
        WebView.setWebContentsDebuggingEnabled(applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0)
        buildStarting()
        actionBar?.apply {
            setDisplayShowHomeEnabled(false)
            setDisplayShowTitleEnabled(false)
            setDisplayShowCustomEnabled(true)
            customView = brandView()
        }
        val root = FrameLayout(this).apply {
            addView(web, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
            addView(starting, FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT)
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

    /** Back from installing or starting i2pd, say so. */
    override fun onResume() {
        super.onResume()
        tellPage()
    }

    /** What the page cannot find out for itself: whether the i2pd app is installed. */
    private fun tellPage() {
        if (loaded != null) web.evaluateJavascript("window.omnidApp?.i2pd(${I2pd.isInstalled(this)})", null)
    }

    override fun onSaveInstanceState(outState: Bundle) {
        super.onSaveInstanceState(outState)
        web.url?.let { outState.putString(KEY_PAGE, it) }
    }

    /** Below Android 13; from 13 the back callback below does this. */
    @Deprecated("Platform back handling; there is no AndroidX here")
    override fun onBackPressed() {
        if (web.canGoBack()) web.goBack() else super.onBackPressed()
    }

    /** From Android 13 (and always from 16), Back reaches the app only through a callback. */
    private fun updateBack() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val wanted = web.canGoBack()
        val current = backCallback as OnBackInvokedCallback?
        if (wanted && current == null) {
            val callback = OnBackInvokedCallback { if (web.canGoBack()) web.goBack() }
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, callback)
            backCallback = callback
        } else if (!wanted && current != null) {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(current)
            backCallback = null
        }
    }

    /** The toolbar's mark and name, on the 16dp keyline. */
    private fun brandView(): View {
        fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()
        return LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(ImageView(this@MainActivity).apply { setImageResource(R.drawable.ic_brand_mark) }, LinearLayout.LayoutParams(dp(30), dp(30)))
            addView(TextView(this@MainActivity).apply {
                text = getString(R.string.app_name)
                setTextColor(getColor(R.color.ink))
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 21.6f)
                typeface = android.graphics.Typeface.create("sans-serif-medium", android.graphics.Typeface.NORMAL)
            }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(12) })
        }
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
                    // Back where it was after a recreation, the search included; the daemon's
                    // own page only.
                    val page = restorePage?.takeIf { it.startsWith("${endpoint.origin}/?embedded") }
                    restorePage = null
                    web.loadUrl(page ?: "${endpoint.origin}/?embedded")
                    loaded = endpoint
                }
                web.visibility = View.VISIBLE
                starting.visibility = View.GONE
                pendingShare?.let { submit(endpoint, it) }
                pendingShare = null
            }
            failure != null -> {
                startingTitle.text = getString(R.string.failed_title)
                startingDetail.text = failure
                startingProgress.visibility = View.INVISIBLE
                retry.visibility = View.VISIBLE
                starting.visibility = View.VISIBLE
            }
            else -> {
                startingTitle.text = getString(R.string.starting)
                startingDetail.text = getString(R.string.starting_detail)
                startingProgress.visibility = View.VISIBLE
                retry.visibility = View.GONE
                starting.visibility = View.VISIBLE
            }
        }
    }

    private fun buildStarting() {
        fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()
        val logo = ImageView(this).apply { setImageResource(R.mipmap.ic_launcher) }
        val onAccent = getColor(R.color.accent_ink)
        startingTitle = TextView(this).apply {
            setTextColor(getColor(R.color.ink))
            // The type ramp's head size (21.6), as on the search page.
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 21.6f)
            typeface = android.graphics.Typeface.create("sans-serif-medium", android.graphics.Typeface.NORMAL)
            gravity = Gravity.CENTER
        }
        startingDetail = TextView(this).apply {
            setTextColor(getColor(R.color.ink_2))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            gravity = Gravity.CENTER
            setLineSpacing(0f, 1.2f)
        }
        startingProgress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            isIndeterminate = true
            indeterminateTintList = ColorStateList.valueOf(getColor(R.color.glow))
        }
        retry = Button(this).apply {
            text = getString(R.string.retry)
            isAllCaps = false
            setTextColor(onAccent)
            backgroundTintList = ColorStateList.valueOf(getColor(R.color.accent))
            visibility = View.GONE
            setOnClickListener {
                stopService(Intent(this@MainActivity, OmnidService::class.java))
                startForegroundService(Intent(this@MainActivity, OmnidService::class.java))
            }
        }
        starting = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding(dp(40), dp(40), dp(40), dp(40))
            setBackgroundColor(getColor(R.color.ground))
            addView(logo, LinearLayout.LayoutParams(dp(88), dp(88)).apply { bottomMargin = dp(20) })
            addView(startingTitle)
            addView(startingDetail, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(6) })
            addView(startingProgress, LinearLayout.LayoutParams(dp(160), dp(4)).apply { topMargin = dp(24) })
            addView(retry, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(16) })
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

    private fun openElsewhere(uri: Uri) = launch(Intent(Intent.ACTION_VIEW, uri))

    private fun launch(intent: Intent) {
        try {
            startActivity(intent)
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, intent.dataString ?: intent.`package`.orEmpty(), Toast.LENGTH_LONG).show()
        }
    }
}
