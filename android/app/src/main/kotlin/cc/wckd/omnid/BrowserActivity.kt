package cc.wckd.omnid

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.EditText
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.PermissionDelegate.ContentPermission
import org.mozilla.geckoview.GeckoSessionSettings
import org.mozilla.geckoview.GeckoView
import org.mozilla.geckoview.StorageController
import org.mozilla.geckoview.WebRequestError
import org.mozilla.geckoview.WebResponse

/**
 * omnid's browser: GeckoView in private mode, every request through the daemon (Tor for onions
 * and the clearnet, i2pd for `.i2p`), each site on Tor circuits of its own. Permissions are
 * refused, downloads are off, and nothing is kept once it closes.
 */
class BrowserActivity : Activity() {
    companion object {
        const val EXTRA_URL = "url"
        private const val KEY_SCREENSHOTS = "browser_screenshots"

        /**
         * The page in view while the activity is recreated for a configuration change (the
         * theme): handed to the new activity instead of closing the private session.
         */
        private var carried: Carried? = null
    }

    private class Carried(val session: GeckoSession, val url: String?, val canGoBack: Boolean, val canGoForward: Boolean)

    private lateinit var geckoView: GeckoView
    private lateinit var address: EditText
    private lateinit var kind: TextView
    private lateinit var back: ImageButton
    private lateinit var reload: ImageButton
    private lateinit var progress: ProgressBar
    private lateinit var message: TextView
    private var runtime: GeckoRuntime? = null
    private var session: GeckoSession? = null
    private var url: String? = null
    private var canGoBack = false
    private var canGoForward = false
    private var loading = false
    /** System Back goes back a page first; registered only while it can (Android 13+). */
    private var backCallback: Any? = null
    /** An address to load once the runtime is ready. */
    private var pending: String? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        applyScreenshotSetting()
        buildViews()
        pending = intent.getStringExtra(EXTRA_URL)
        Browser.ready(this).accept(
            { ready -> start(checkNotNull(ready)) },
            { error -> showFailure(error) },
        )
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.getStringExtra(EXTRA_URL)?.let(::load)
    }

    override fun onDestroy() {
        val current = session
        if (current != null && isChangingConfigurations) {
            // Recreated, not closed: the next instance takes the page over.
            geckoView.releaseSession()
            carried = Carried(current, url, canGoBack, canGoForward)
        } else {
            // Closing the last private session makes Gecko drop its cookies and storage.
            current?.close()
        }
        session = null
        super.onDestroy()
    }

    /** Below Android 13; from 13 the back callback does this. */
    @Deprecated("Platform back handling; there is no AndroidX here")
    override fun onBackPressed() {
        val current = session
        if (current != null && canGoBack) current.goBack() else super.onBackPressed()
    }

    /** From Android 13 (and always from 16), Back reaches the app only through a callback. */
    private fun updateBack() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val current = backCallback as OnBackInvokedCallback?
        if (canGoBack && current == null) {
            val callback = OnBackInvokedCallback { session?.goBack() }
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, callback)
            backCallback = callback
        } else if (!canGoBack && current != null) {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(current)
            backCallback = null
        }
    }

    private fun start(ready: GeckoRuntime) {
        runtime = ready
        val kept = carried
        carried = null
        if (kept != null) {
            adopt(kept)
            return
        }
        openSession(ready)
        val first = pending
        pending = null
        if (first != null) load(first) else focusAddress()
    }

    /** Takes over the page a recreated instance was showing. */
    private fun adopt(kept: Carried) {
        session = kept.session.apply {
            navigationDelegate = navigation
            progressDelegate = progressListener
            contentDelegate = content
            permissionDelegate = DenyEverything
        }
        geckoView.setSession(kept.session)
        url = kept.url
        canGoBack = kept.canGoBack
        canGoForward = kept.canGoForward
        setEnabled(back, canGoBack)
        updateBack()
        showAddress()
        showKind(url, secure = null)
        pending = null
        message.visibility = View.GONE
        geckoView.visibility = View.VISIBLE
    }

    /** The full address while editing, the short form otherwise. */
    private fun showAddress() {
        val current = url.orEmpty()
        address.setText(if (address.hasFocus()) current else BrowserConfig.displayAddress(current))
    }

    private fun openSession(runtime: GeckoRuntime) {
        val settings = GeckoSessionSettings.Builder()
            .usePrivateMode(true)
            .useTrackingProtection(true)
            .allowJavascript(true)
            .build()
        session = GeckoSession(settings).apply {
            navigationDelegate = navigation
            progressDelegate = progressListener
            contentDelegate = content
            permissionDelegate = DenyEverything
            open(runtime)
        }
        geckoView.setSession(checkNotNull(session))
        message.visibility = View.GONE
        geckoView.visibility = View.VISIBLE
    }

    private fun load(target: String) {
        val current = session
        if (current == null) {
            pending = target
            return
        }
        val uri = Uri.parse(target)
        if (BrowserConfig.isWebPage(uri.scheme, uri.host)) current.loadUri(target) else refuse(target)
    }

    private fun refuse(target: String) =
        Toast.makeText(this, getString(R.string.browser_refused, target), Toast.LENGTH_LONG).show()

    private val navigation = object : GeckoSession.NavigationDelegate {
        override fun onLocationChange(
            session: GeckoSession,
            url: String?,
            perms: MutableList<ContentPermission>,
            hasUserGesture: Boolean,
        ) {
            this@BrowserActivity.url = url
            if (!address.hasFocus()) showAddress()
            showKind(url, secure = null)
        }

        override fun onCanGoBack(session: GeckoSession, canGoBack: Boolean) {
            this@BrowserActivity.canGoBack = canGoBack
            setEnabled(back, canGoBack)
            updateBack()
        }

        override fun onCanGoForward(session: GeckoSession, canGoForward: Boolean) {
            this@BrowserActivity.canGoForward = canGoForward
        }

        override fun onLoadRequest(
            session: GeckoSession,
            request: GeckoSession.NavigationDelegate.LoadRequest,
        ): GeckoResult<AllowOrDeny>? {
            val uri = Uri.parse(request.uri)
            return when (BrowserConfig.target(uri.scheme, uri.host)) {
                BrowserConfig.Target.LOAD -> GeckoResult.allow()
                BrowserConfig.Target.ELSEWHERE -> {
                    openElsewhere(uri)
                    GeckoResult.deny()
                }
                BrowserConfig.Target.REFUSE -> {
                    refuse(request.uri)
                    GeckoResult.deny()
                }
            }
        }

        // Pages that open windows get the new page in this one: there are no tabs.
        override fun onNewSession(session: GeckoSession, uri: String): GeckoResult<GeckoSession>? {
            load(uri)
            return null
        }

        override fun onLoadError(session: GeckoSession, uri: String?, error: WebRequestError): GeckoResult<String> =
            GeckoResult.fromValue(errorPage(uri, error))
    }

    private val progressListener = object : GeckoSession.ProgressDelegate {
        override fun onPageStart(session: GeckoSession, url: String) {
            setLoading(true)
            progress.progress = 0
        }

        override fun onPageStop(session: GeckoSession, success: Boolean) = setLoading(false)

        override fun onProgressChange(session: GeckoSession, progress: Int) {
            this@BrowserActivity.progress.progress = progress
        }

        override fun onSecurityChange(session: GeckoSession, securityInfo: GeckoSession.ProgressDelegate.SecurityInformation) =
            showKind(url, securityInfo.isSecure)
    }

    private val content = object : GeckoSession.ContentDelegate {
        override fun onCloseRequest(session: GeckoSession) = finish()

        override fun onExternalResponse(session: GeckoSession, response: WebResponse) {
            Toast.makeText(this@BrowserActivity, R.string.browser_no_downloads, Toast.LENGTH_LONG).show()
        }

        override fun onCrash(session: GeckoSession) = recover()

        override fun onKill(session: GeckoSession) = recover()
    }

    /** The page's process died: a new session, back where it was. */
    private fun recover() {
        val ready = runtime ?: return
        val last = url
        session?.close()
        openSession(ready)
        if (last != null) load(last)
    }

    /** Camera, microphone, location, notifications, storage: none of it, from any site. */
    private object DenyEverything : GeckoSession.PermissionDelegate {
        override fun onContentPermissionRequest(session: GeckoSession, perm: ContentPermission): GeckoResult<Int> =
            GeckoResult.fromValue(ContentPermission.VALUE_DENY)

        override fun onMediaPermissionRequest(
            session: GeckoSession,
            uri: String,
            video: Array<out GeckoSession.PermissionDelegate.MediaSource>?,
            audio: Array<out GeckoSession.PermissionDelegate.MediaSource>?,
            callback: GeckoSession.PermissionDelegate.MediaCallback,
        ) = callback.reject()

        override fun onAndroidPermissionsRequest(
            session: GeckoSession,
            permissions: Array<out String>?,
            callback: GeckoSession.PermissionDelegate.Callback,
        ) = callback.reject()
    }

    /**
     * Closes the session and its data, and gives every site new Tor circuits: nothing seen so far
     * can link what comes next.
     */
    private fun newIdentity() {
        val ready = runtime ?: return
        session?.close()
        session = null
        url = null
        address.setText("")
        showKind(null, secure = null)
        ready.storageController.clearData(StorageController.ClearFlags.ALL)
        Thread {
            Native.newIdentity()
            runOnUiThread {
                openSession(ready)
                focusAddress()
                Toast.makeText(this, R.string.browser_new_identity_done, Toast.LENGTH_SHORT).show()
            }
        }.start()
    }

    private fun setLoading(loading: Boolean) {
        this.loading = loading
        progress.visibility = if (loading) View.VISIBLE else View.INVISIBLE
        reload.setImageResource(if (loading) R.drawable.ic_stop else R.drawable.ic_reload)
        reload.contentDescription = getString(if (loading) R.string.browser_stop else R.string.browser_reload)
    }

    private fun setEnabled(button: ImageButton, enabled: Boolean) {
        button.isEnabled = enabled
        button.imageAlpha = if (enabled) 255 else 80
    }

    /** Which network the page came over, and whether the connection to it is encrypted. */
    private fun showKind(url: String?, secure: Boolean?) {
        val uri = url?.let(Uri::parse)
        val host = uri?.host.orEmpty()
        val (label, color, sealed) = when {
            uri == null || !BrowserConfig.isWebPage(uri.scheme, host) -> Triple("", R.color.muted, true)
            host.endsWith(".onion") -> Triple(getString(R.string.browser_kind_onion), R.color.onion, true)
            host.endsWith(".i2p") -> Triple("I2P", R.color.i2p, true)
            secure == false || uri.scheme == "http" -> Triple(getString(R.string.browser_tor_plain), R.color.warn, false)
            else -> Triple("Tor", R.color.ok, true)
        }
        kind.text = label
        kind.setTextColor(getColor(color))
        val lock = checkNotNull(getDrawable(if (sealed) R.drawable.ic_lock else R.drawable.ic_lock_open)).mutate()
        val size = (16 * resources.displayMetrics.density).toInt()
        lock.setBounds(0, 0, size, size)
        lock.setTint(getColor(color))
        kind.setCompoundDrawablesRelative(lock, null, null, null)
        kind.visibility = if (label.isEmpty()) View.GONE else View.VISIBLE
    }

    private fun showFailure(error: Throwable?) {
        message.text = getString(R.string.browser_unsafe, error?.message ?: error.toString())
        message.visibility = View.VISIBLE
        geckoView.visibility = View.GONE
    }

    private fun focusAddress() {
        address.requestFocus()
        getSystemService(InputMethodManager::class.java)?.showSoftInput(address, InputMethodManager.SHOW_IMPLICIT)
    }

    private fun go() {
        val target = BrowserConfig.address(address.text.toString())
        if (target == null) {
            Toast.makeText(this, R.string.browser_not_an_address, Toast.LENGTH_SHORT).show()
            return
        }
        // Focus to the page, so the address bar shows where the page went.
        geckoView.requestFocus()
        getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(address.windowToken, 0)
        load(target)
    }

    private fun openElsewhere(uri: Uri) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, uri.toString(), Toast.LENGTH_LONG).show()
        }
    }

    private fun showMenu(anchor: View) {
        PopupMenu(this, anchor).apply {
            menu.add(0, 4, 0, R.string.browser_forward).isEnabled = canGoForward
            menu.add(0, 1, 0, R.string.browser_new_identity)
            menu.add(0, 2, 1, R.string.browser_allow_screenshots).apply {
                isCheckable = true
                isChecked = screenshotsAllowed()
            }
            menu.add(0, 3, 2, R.string.browser_close)
            setOnMenuItemClickListener { item ->
                when (item.itemId) {
                    1 -> newIdentity()
                    2 -> {
                        prefs().edit().putBoolean(KEY_SCREENSHOTS, !item.isChecked).apply()
                        applyScreenshotSetting()
                    }
                    3 -> finish()
                    4 -> session?.goForward()
                }
                true
            }
        }.show()
    }

    private fun prefs() = getSharedPreferences(CrawlPolicy.PREFS, MODE_PRIVATE)

    private fun screenshotsAllowed() = prefs().getBoolean(KEY_SCREENSHOTS, false)

    /** Off by default: screenshots and the recent-apps thumbnail would keep what was on screen. */
    private fun applyScreenshotSetting() {
        if (screenshotsAllowed()) {
            window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
        } else {
            window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        }
    }

    private fun buildViews() {
        fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()
        val ripple = TypedValue().also { theme.resolveAttribute(android.R.attr.selectableItemBackgroundBorderless, it, true) }.resourceId
        fun button(icon: Int, description: Int, onClick: (View) -> Unit) = ImageButton(this).apply {
            setImageResource(icon)
            setBackgroundResource(ripple)
            contentDescription = getString(description)
            setOnClickListener(onClick)
        }
        back = button(R.drawable.ic_back, R.string.browser_back) { session?.goBack() }
        reload = button(R.drawable.ic_reload, R.string.browser_reload) { if (loading) session?.stop() else session?.reload() }
        val menu = button(R.drawable.ic_more, R.string.browser_menu, ::showMenu)
        setEnabled(back, false)
        kind = TextView(this).apply {
            gravity = Gravity.CENTER_VERTICAL
            includeFontPadding = false
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            compoundDrawablePadding = dp(4)
            setPadding(0, 0, dp(6), 0)
            visibility = View.GONE
        }
        address = EditText(this).apply {
            hint = getString(R.string.browser_address_hint)
            isSingleLine = true
            ellipsize = TextUtils.TruncateAt.END
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setTextColor(getColor(R.color.ink))
            setHintTextColor(getColor(R.color.muted))
            background = null
            setPadding(0, 0, 0, 0)
            inputType = InputType.TYPE_TEXT_VARIATION_URI or InputType.TYPE_CLASS_TEXT
            // The keyboard must not learn or sync what is typed here.
            imeOptions = EditorInfo.IME_ACTION_GO or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            // The full address to edit, selected; the short form again when done.
            setOnFocusChangeListener { _, focused ->
                showAddress()
                if (focused) selectAll()
            }
            setOnEditorActionListener { _, action, event ->
                val enter = event?.keyCode == KeyEvent.KEYCODE_ENTER && event.action == KeyEvent.ACTION_DOWN
                if (action == EditorInfo.IME_ACTION_GO || enter) {
                    go()
                    true
                } else {
                    false
                }
            }
        }
        // The address pill: where the page came from and how, then the address.
        val pill = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            // Centre the badge and the address on each other, not on their text baselines.
            isBaselineAligned = false
            setBackgroundResource(R.drawable.bg_address)
            setPadding(dp(14), 0, dp(12), 0)
            addView(kind)
            addView(address, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            setOnClickListener { focusAddress() }
        }
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(getColor(R.color.ground))
            setPadding(dp(2), dp(6), dp(2), dp(6))
            addView(back, LinearLayout.LayoutParams(dp(48), dp(48)))
            addView(pill, LinearLayout.LayoutParams(0, dp(48), 1f).apply { marginStart = dp(2); marginEnd = dp(2) })
            addView(reload, LinearLayout.LayoutParams(dp(48), dp(48)))
            addView(menu, LinearLayout.LayoutParams(dp(48), dp(48)))
        }
        progress = ProgressBar(this, null, android.R.attr.progressBarStyleHorizontal).apply {
            max = 100
            visibility = View.INVISIBLE
            progressTintList = ColorStateList.valueOf(getColor(R.color.glow))
            progressBackgroundTintList = ColorStateList.valueOf(getColor(R.color.ground))
        }
        geckoView = GeckoView(this).apply { isFocusableInTouchMode = true }
        message = TextView(this).apply {
            gravity = Gravity.CENTER
            setTextColor(getColor(R.color.ink_2))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            setPadding(dp(32), dp(32), dp(32), dp(32))
            visibility = View.GONE
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(getColor(R.color.ground))
            addView(bar, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(progress, LinearLayout.LayoutParams.MATCH_PARENT, dp(2))
            addView(message, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(geckoView, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
            // Edge to edge from Android 15: keep the toolbar and page clear of the system bars.
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
    }

    /** A page saying why a load failed, as a data: URL Gecko shows in place of the page. */
    private fun errorPage(uri: String?, error: WebRequestError): String {
        val reason = getString(
            when {
                // omnid's proxy refuses a .i2p site when no I2P router answers.
                error.code == WebRequestError.ERROR_CONNECTION_REFUSED && BrowserConfig.isI2p(uri) -> R.string.browser_error_no_i2p_router
                // omnid's proxy reports a host Tor or I2P could not reach as an unknown host.
                error.code == WebRequestError.ERROR_UNKNOWN_HOST -> R.string.browser_error_unreachable
                error.code == WebRequestError.ERROR_HTTPS_ONLY -> R.string.browser_error_https_only
                error.code == WebRequestError.ERROR_NET_TIMEOUT -> R.string.browser_error_timeout
                error.category == WebRequestError.ERROR_CATEGORY_PROXY -> R.string.browser_error_proxy
                error.category == WebRequestError.ERROR_CATEGORY_SECURITY -> R.string.browser_error_security
                error.category == WebRequestError.ERROR_CATEGORY_URI -> R.string.browser_error_uri
                else -> R.string.browser_error_network
            },
        )
        val escape = { text: String -> TextUtils.htmlEncode(text) }
        // The brand's dark scheme always: Gecko's fingerprinting protection reports a light one.
        val html = "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>" +
            "<body style='font:15px/1.55 sans-serif;margin:0;padding:3rem 1.5rem;background:#0B0D1F;color:#B2BBDF;min-height:100vh'>" +
            "<h1 style='margin:0 0 .5rem;font-size:21.6px;font-weight:650;color:#EEF2FF'>${escape(getString(R.string.browser_error_title))}</h1>" +
            "<p style='margin:0 0 1.25rem'>${escape(reason)}</p>" +
            "<p style='margin:0;word-break:break-all;color:#8690BD;font:12.5px/1.5 monospace'>${escape(uri.orEmpty())}</p>" +
            "<p style='margin:.5rem 0 0;color:#8690BD;font-size:12.5px'>${escape(getString(R.string.browser_error_code, "0x" + Integer.toHexString(error.code)))}</p>" +
            // A .i2p site over HTTPS: reload it over HTTP with an exception for this site, before
            // the error is drawn. Gecko gives error pages that call; without it the error shows.
            (if (BrowserConfig.retriesOverHttp(uri)) "<script>if(document.reloadWithHttpsOnlyException){document.body.hidden=true;document.reloadWithHttpsOnlyException()}</script>" else "")
        return "data:text/html;charset=utf-8," + Uri.encode(html)
    }
}
