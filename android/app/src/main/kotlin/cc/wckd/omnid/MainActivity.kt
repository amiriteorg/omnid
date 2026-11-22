package cc.wckd.omnid

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.app.ActivityOptions
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.RippleDrawable
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
    companion object {
        /** Words to search for, from the browser's address bar. */
        const val EXTRA_QUERY = "query"
        private const val KEY_PAGE = "page"
        /** The page's link to the i2pd app (app.js): opens it, or its F-Droid page. */
        private const val I2PD_LINK = "omnid:i2pd"

        /** The page's link to Settings → I2P router (app.js). */
        private const val I2P_ROUTER_LINK = "omnid:i2p-router"
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
    private lateinit var drawnWith: Settings.Theme
    /** Settings → Search as the page was loaded with: networks and languages. */
    private var searchSettings: Pair<Set<String>, String>? = null
    /** A search the browser's address bar asked for before the page was ready. */
    private var pendingQuery: String? = null

    override fun attachBaseContext(base: Context) {
        super.attachBaseContext(base)
        Appearance.apply(this, base)
    }

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        drawnWith = Appearance.chosen(this)
        restorePage = savedInstanceState?.getString(KEY_PAGE)
        Appearance.applyScreenshots(this)
        web = PrivateWebView(this).apply {
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
                    // The daemon's own pages load here, and nothing else on this phone: the
                    // page's cookie (the API token) would go to any loopback port it loaded.
                    if (uri.host == "127.0.0.1" && uri.port == loaded?.port) return false
                    if (uri.host == "127.0.0.1" || uri.host.equals("localhost", ignoreCase = true)) return true
                    if (uri.toString() == I2PD_LINK) {
                        launch(I2pd.intent(this@MainActivity))
                    } else if (uri.toString() == I2P_ROUTER_LINK) {
                        startActivity(Intent(this@MainActivity, SettingsActivity::class.java).putExtra(SettingsActivity.EXTRA_I2P_ROUTER, true))
                    } else if (uri.scheme.equals("magnet", ignoreCase = true)) {
                        // A result: already in the index, so only the torrent app, after asking.
                        askTorrentApp(uri.toString(), offerIndex = false)
                    } else if (uri.scheme.equals("nostr", ignoreCase = true)) {
                        askNostrApp(uri.toString())
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
        takeQuery(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        takeShare(intent)
        takeQuery(intent)
    }

    /** Words typed into the browser's address bar that were not an address: search for them. */
    private fun takeQuery(intent: Intent?) {
        val query = intent?.getStringExtra(EXTRA_QUERY)?.takeIf { it.isNotBlank() } ?: return
        intent.removeExtra(EXTRA_QUERY)
        val endpoint = OmnidService.endpoint
        if (endpoint == null || loaded != endpoint) pendingQuery = query else loadSearch(endpoint, query)
    }

    /** The search page as Settings → Search has it, searching [query] if there is one. */
    private fun loadSearch(endpoint: OmnidService.Endpoint, query: String?) {
        val wanted = wantedSearch()
        searchSettings = wanted
        web.loadUrl(Settings.searchPage(endpoint.origin, wanted.first, wanted.second, query))
    }

    /** Settings → Search now: the networks a search starts with, and the languages ranked first. */
    private fun wantedSearch(): Pair<Set<String>, String> {
        val prefs = prefs()
        return Settings.networks(prefs.getString(Settings.SEARCH_NETWORKS, null)) to prefs.getString(Settings.SEARCH_LANGUAGES, "").orEmpty()
    }

    /** What a search page's address says of the same, as [Settings.searchPage] wrote it. */
    private fun searchOf(page: String): Pair<Set<String>, String> {
        val uri = Uri.parse(page)
        return Settings.networks(uri.getQueryParameter("networks")) to uri.getQueryParameter("lang").orEmpty()
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
        Appearance.applyScreenshots(this)
        if (Appearance.chosen(this) != drawnWith) {
            recreate()
            return
        }
        // Settings → Search changed: the page starts again with them, the search kept.
        val endpoint = loaded
        if (endpoint != null && searchSettings != null && wantedSearch() != searchSettings) {
            loadSearch(endpoint, web.url?.let { Uri.parse(it).getQueryParameter("q") })
        }
        tellPage()
        // The browser's tabs may have opened or closed meanwhile.
        invalidateOptionsMenu()
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
                // The wordmark: Space Mono Bold, set tight.
                typeface = resources.getFont(R.font.space_mono_bold)
                letterSpacing = -0.04f
            }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginStart = dp(12) })
        }
    }

    override fun onCreateOptionsMenu(menu: Menu): Boolean {
        // Back to the browser's open tabs, while there are any.
        menu.add(Menu.NONE, R.string.browser_open_tabs, Menu.NONE, R.string.browser_open_tabs).apply {
            setShowAsAction(MenuItem.SHOW_AS_ACTION_ALWAYS)
            actionView = TabButton(this@MainActivity).apply {
                setOnClickListener { startActivity(Intent(this@MainActivity, BrowserActivity::class.java)) }
            }
        }
        menu.add(Menu.NONE, R.string.crawl_anytime, Menu.NONE, R.string.crawl_anytime).isCheckable = true
        menu.add(Menu.NONE, R.string.settings, Menu.NONE, R.string.settings)
        menu.add(Menu.NONE, R.string.stop_omnid, Menu.NONE, R.string.stop_omnid)
        return true
    }

    override fun onPrepareOptionsMenu(menu: Menu): Boolean {
        menu.findItem(R.string.browser_open_tabs).apply {
            isVisible = BrowserActivity.openTabs > 0
            (actionView as TabButton).count = BrowserActivity.openTabs
        }
        menu.findItem(R.string.crawl_anytime).isChecked = prefs().getBoolean(CrawlPolicy.KEY_ANYTIME, false)
        return true
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean = when (item.itemId) {
        R.string.crawl_anytime -> {
            prefs().edit().putBoolean(CrawlPolicy.KEY_ANYTIME, !item.isChecked).apply()
            true
        }
        R.string.settings -> {
            startActivity(Intent(this, SettingsActivity::class.java))
            true
        }
        R.string.stop_omnid -> {
            BrowserActivity.closeAll()
            stopService(Intent(this, OmnidService::class.java))
            finishAndRemoveTask()
            true
        }
        else -> super.onOptionsItemSelected(item)
    }

    private fun prefs() = getSharedPreferences(CrawlPolicy.PREFS, MODE_PRIVATE)

    private fun restartService() {
        stopService(Intent(this, OmnidService::class.java))
        startForegroundService(Intent(this, OmnidService::class.java))
    }

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
                    val query = pendingQuery
                    pendingQuery = null
                    // The page as it was, unless Settings → Search changed since it loaded.
                    if (page != null && query == null && searchOf(page) == wantedSearch()) {
                        web.loadUrl(page)
                        searchSettings = searchOf(page)
                    } else if (page != null && query == null) {
                        loadSearch(endpoint, Uri.parse(page).getQueryParameter("q"))
                    } else {
                        loadSearch(endpoint, query)
                    }
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
            // The type ramp's head size (21.6), in the search page's Space Mono.
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 21.6f)
            typeface = resources.getFont(R.font.space_mono_bold)
            letterSpacing = -0.04f
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
            // The page's primary button: a square accent block, its label in capitals.
            typeface = resources.getFont(R.font.space_mono_bold)
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
            letterSpacing = 0.04f
            setTextColor(onAccent)
            background = RippleDrawable(ColorStateList.valueOf(getColor(R.color.ink)), ColorDrawable(getColor(R.color.accent)), null)
            minHeight = dp(48)
            setPadding(dp(20), 0, dp(20), 0)
            visibility = View.GONE
            setOnClickListener { restartService() }
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

/**
 * The search page's WebView, whose text fields the keyboard must not learn from: what is searched
 * for here stays out of its suggestions and anything it syncs.
 */
internal class PrivateWebView(context: android.content.Context) : WebView(context) {
    override fun onCreateInputConnection(outAttrs: android.view.inputmethod.EditorInfo): android.view.inputmethod.InputConnection? {
        val connection = super.onCreateInputConnection(outAttrs)
        outAttrs.imeOptions = outAttrs.imeOptions or android.view.inputmethod.EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
        return connection
    }
}
