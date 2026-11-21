package cc.wckd.omnid

import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.PersistableBundle
import android.text.Editable
import android.text.InputType
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextUtils
import android.text.TextWatcher
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.text.style.StyleSpan
import android.util.Base64
import android.util.TypedValue
import android.view.Gravity
import android.view.KeyEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.BaseAdapter
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.LinearLayout
import android.widget.ListView
import android.widget.PopupMenu
import android.widget.ProgressBar
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import java.lang.ref.WeakReference
import org.mozilla.geckoview.AllowOrDeny
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoSession
import org.mozilla.geckoview.GeckoSession.ContentDelegate.ContextElement
import org.mozilla.geckoview.GeckoSession.PermissionDelegate.ContentPermission
import org.mozilla.geckoview.GeckoSessionSettings
import org.mozilla.geckoview.GeckoView
import org.mozilla.geckoview.StorageController
import org.mozilla.geckoview.WebRequestError
import org.mozilla.geckoview.WebResponse

/**
 * omnid's browser: tabs of GeckoView sessions in private mode, every request through the daemon
 * (Tor for onions and the clearnet, i2pd for `.i2p`, the Hyphanet node for Hyphanet), each site
 * on Tor circuits of its own. Permissions are refused, downloads are off, and nothing but
 * bookmarks is kept once it closes. It runs in a task of its own, so the search page and the
 * open tabs are each one Back or one tap from the other.
 */
class BrowserActivity : Activity() {
    companion object {
        const val EXTRA_URL = "url"
        private const val KEY_SCREENSHOTS = "browser_screenshots"
        /** Enough for a session of reading; every tab keeps its page in memory. */
        private const val MAX_TABS = 16

        private const val MENU_FORWARD = 1
        private const val MENU_NEW_TAB = 2
        private const val MENU_FIND = 3
        private const val MENU_BOOKMARK = 4
        private const val MENU_BOOKMARKS = 5
        private const val MENU_COPY = 6
        private const val MENU_LEVEL = 7
        private const val MENU_IDENTITY = 8
        private const val MENU_SCREENSHOTS = 9
        private const val MENU_CLOSE = 10

        /** How many tabs are open, for the search page's way back to them. */
        var openTabs = 0
            private set

        /**
         * The tabs while the activity is recreated for a configuration change (the theme): handed
         * to the new activity instead of closing the private sessions.
         */
        private var carried: Carried? = null
        private var running: WeakReference<BrowserActivity>? = null
        /** Space Mono Bold as a data: URI, for the error page, which loads nothing from outside. */
        private var errorFont: String? = null

        /** Closes every tab, as Close browser does: for when OMNId stops. */
        fun closeAll() {
            running?.get()?.finishAndRemoveTask()
        }
    }

    /** One tab: its session, where it came from, and what the toolbar shows while it is shown. */
    private class Tab(var session: GeckoSession, var opener: Tab?, val fromSearch: Boolean, var url: String?) {
        var title: String? = null
        var canGoBack = false
        var canGoForward = false
        var loading = false
        var progress = 0
        var secure: Boolean? = null
    }

    private class Carried(val tabs: TabList<Tab>, val listing: Boolean)

    private lateinit var geckoView: GeckoView
    private lateinit var bar: View
    private lateinit var address: EditText
    private lateinit var kind: TextView
    private lateinit var back: ImageButton
    private lateinit var reload: ImageButton
    private lateinit var tabButton: TabButton
    private lateinit var progress: ProgressBar
    private lateinit var message: TextView
    private lateinit var findBar: View
    private lateinit var findText: EditText
    private lateinit var findCount: TextView
    private lateinit var tabList: View
    private lateinit var tabListTitle: TextView
    private lateinit var tabRows: BaseAdapter
    private var runtime: GeckoRuntime? = null
    private var tabs = TabList<Tab>(MAX_TABS)
    private var level = SecurityLevel.STANDARD
    /** Addresses to open once the runtime is ready. */
    private val pending = mutableListOf<String>()
    /** Counts finds, so a slow answer to an earlier keystroke does not overwrite the latest. */
    private var finds = 0
    /** Typing finds once it pauses: finds that overlap can come back uncounted. */
    private val findTyped = Runnable { find(again = false) }

    private val shown: Tab? get() = tabs.shown

    /** Space Mono, the search page's face: regular for addresses, bold for labels and names. */
    private val mono by lazy { resources.getFont(R.font.space_mono) }
    private val monoBold by lazy { resources.getFont(R.font.space_mono_bold) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        running = WeakReference(this)
        level = SecurityLevel.from(prefs().getString(SecurityLevel.KEY, null))
        applyScreenshotSetting()
        buildViews()
        // A recreated activity has its tabs already, and the intent that opened the first.
        if (savedInstanceState == null) intent.getStringExtra(EXTRA_URL)?.let(pending::add)
        registerBack()
        Browser.ready(this).accept(
            { ready -> start(checkNotNull(ready)) },
            { error -> showFailure(error) },
        )
    }

    /** A result opened from the search page, in a tab of its own. */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        val url = intent.getStringExtra(EXTRA_URL) ?: return
        if (runtime == null) pending.add(url) else openTab(url, fromSearch = true)
    }

    override fun onStart() {
        super.onStart()
        shown?.session?.takeIf { it.isOpen }?.setActive(true)
    }

    override fun onStop() {
        shown?.session?.takeIf { it.isOpen }?.setActive(false)
        super.onStop()
    }

    override fun onDestroy() {
        geckoView.releaseSession()
        if (isChangingConfigurations && tabs.size > 0) {
            // Recreated, not closed: the next instance takes the tabs over.
            carried = Carried(tabs, tabList.visibility == View.VISIBLE)
        } else {
            // Closing the last private session makes Gecko drop its cookies and storage.
            tabs.all.forEach { it.session.close() }
            tabs.clear()
            openTabs = 0
        }
        if (running?.get() === this) running = null
        super.onDestroy()
    }

    /** Below Android 13; from 13 the back callback does this. */
    @Deprecated("Platform back handling; there is no AndroidX here")
    override fun onBackPressed() = goBack()

    /** From Android 13 (and always from 16), Back reaches the app only through a callback. */
    private fun registerBack() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, OnBackInvokedCallback { goBack() })
    }

    /**
     * Back: out of find in page or the tab list, then a page back, then to the tab this one was
     * opened from. A tab opened from the search page closes and the search page shows; any other
     * stays open behind it.
     */
    private fun goBack() {
        val tab = shown
        when {
            findBar.visibility == View.VISIBLE -> closeFind()
            tabList.visibility == View.VISIBLE && tab != null -> showTabList(false)
            tab == null -> finishAndRemoveTask()
            tab.canGoBack -> tab.session.goBack()
            tab.opener != null -> closeTab(tab, then = tab.opener)
            tab.fromSearch -> {
                closeTab(tab)
                if (tabs.size > 0) moveTaskToBack(true)
            }
            else -> moveTaskToBack(true)
        }
    }

    private fun start(ready: GeckoRuntime) {
        runtime = ready
        carried?.let { kept ->
            carried = null
            tabs = kept.tabs
            tabs.all.forEach { attach(it.session) }
            tabs.shown?.let(::showTab)
            if (kept.listing) showTabList(true)
        }
        pending.forEach { openTab(it, fromSearch = true) }
        pending.clear()
        if (tabs.size == 0) openTab(null)
    }

    private fun newSession(url: String?): GeckoSession {
        val settings = GeckoSessionSettings.Builder()
            .usePrivateMode(true)
            .useTrackingProtection(true)
            .allowJavascript(level.allowsJavascript(url))
            .build()
        return GeckoSession(settings).also(::attach)
    }

    private fun attach(session: GeckoSession) {
        session.navigationDelegate = navigation
        session.progressDelegate = progressListener
        session.contentDelegate = content
        session.permissionDelegate = DenyEverything
    }

    private fun tabOf(session: GeckoSession): Tab? = tabs.all.firstOrNull { it.session === session }

    /** Opens [url], or an empty tab to type an address into, next to the shown tab. */
    private fun openTab(url: String?, opener: Tab? = null, fromSearch: Boolean = false, background: Boolean = false) {
        val ready = runtime ?: return
        if (tabs.isFull) return tooManyTabs()
        val tab = Tab(newSession(url).also { it.open(ready) }, opener, fromSearch, url)
        tabs.add(tab, background)
        if (background) {
            tab.session.setActive(false)
            showCount()
        } else {
            showTab(tab)
        }
        if (url != null) load(tab, url) else focusAddress()
    }

    private fun tooManyTabs() {
        Toast.makeText(this, getString(R.string.browser_tabs_full, MAX_TABS), Toast.LENGTH_LONG).show()
        showTabList(true)
    }

    /** Puts [tab] on screen, with the toolbar in its state. */
    private fun showTab(tab: Tab) {
        if (findBar.visibility == View.VISIBLE) closeFind()
        val previous = geckoView.session
        if (previous !== tab.session) {
            previous?.takeIf { it.isOpen }?.setActive(false)
            geckoView.setSession(tab.session)
            if (tab.session.isOpen) tab.session.setActive(true)
        }
        tabs.select(tab)
        message.visibility = View.GONE
        showTabList(false)
    }

    /** Closes [tab]; [then] is shown next if it is still open, else the tab that takes its place. */
    private fun closeTab(tab: Tab, then: Tab? = null) {
        if (geckoView.session === tab.session) geckoView.releaseSession()
        tab.session.close()
        tabs.all.forEach { if (it.opener === tab) it.opener = null }
        if (tabs.close(tab) == null) {
            finishAndRemoveTask()
            return
        }
        then?.takeIf { it in tabs.all }?.let(tabs::select)
        val listing = tabList.visibility == View.VISIBLE
        val current = checkNotNull(shown)
        if (geckoView.session !== current.session) showTab(current) else render()
        if (listing) showTabList(true)
    }

    /** The toolbar for the shown tab. */
    private fun render() {
        val tab = shown
        showAddress()
        showKind(tab?.url, tab?.secure)
        setEnabled(back, tab?.canGoBack == true)
        setLoading(tab?.loading == true)
        progress.progress = tab?.progress ?: 0
        showCount()
    }

    private fun showCount() {
        openTabs = tabs.size
        tabButton.count = tabs.size
        if (tabList.visibility == View.VISIBLE) {
            tabListTitle.text = resources.getQuantityString(R.plurals.browser_tab_count, tabs.size, tabs.size)
            tabRows.notifyDataSetChanged()
        }
    }

    /** The full address while editing, the short form otherwise. */
    private fun showAddress() {
        val current = shown?.url.orEmpty()
        address.setText(if (address.hasFocus()) current else BrowserConfig.displayAddress(current))
    }

    private fun load(tab: Tab, target: String) {
        val uri = Uri.parse(target)
        if (!BrowserConfig.isWebPage(uri.scheme, uri.host)) return refuse(target)
        allowScripts(tab.session, target)
        tab.session.loadUri(target)
    }

    /** Whether the page [session] loads next may run JavaScript, by the security level; set before it arrives. */
    private fun allowScripts(session: GeckoSession, url: String?) {
        val allowed = level.allowsJavascript(url)
        if (session.settings.allowJavascript != allowed) session.settings.allowJavascript = allowed
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
            val tab = tabOf(session) ?: return
            tab.url = url
            tab.secure = null
            if (tab !== shown) return
            if (!address.hasFocus()) showAddress()
            showKind(url, secure = null)
        }

        override fun onCanGoBack(session: GeckoSession, canGoBack: Boolean) {
            val tab = tabOf(session) ?: return
            tab.canGoBack = canGoBack
            if (tab === shown) setEnabled(back, canGoBack)
        }

        override fun onCanGoForward(session: GeckoSession, canGoForward: Boolean) {
            tabOf(session)?.canGoForward = canGoForward
        }

        override fun onLoadRequest(
            session: GeckoSession,
            request: GeckoSession.NavigationDelegate.LoadRequest,
        ): GeckoResult<AllowOrDeny>? {
            val uri = Uri.parse(request.uri)
            return when (BrowserConfig.target(uri.scheme, uri.host)) {
                BrowserConfig.Target.LOAD -> {
                    // A page for a new tab gets a session of its own, set up in onNewSession.
                    val here = request.target == GeckoSession.NavigationDelegate.TARGET_WINDOW_CURRENT
                    if (here && BrowserConfig.isWebPage(uri.scheme, uri.host)) allowScripts(session, request.uri)
                    GeckoResult.allow()
                }
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

        // Links that open a window, and window.open after a tap, get a tab next to their page.
        override fun onNewSession(session: GeckoSession, uri: String): GeckoResult<GeckoSession>? {
            if (tabs.isFull) {
                tooManyTabs()
                return null
            }
            // Unopened: Gecko opens it, already holding the page.
            val tab = Tab(newSession(uri), tabOf(session), fromSearch = false, url = uri)
            tabs.add(tab)
            showTab(tab)
            return GeckoResult.fromValue(tab.session)
        }

        override fun onLoadError(session: GeckoSession, uri: String?, error: WebRequestError): GeckoResult<String> {
            // The error page's one script reloads a .i2p site or Hyphanet page over HTTP, at any
            // level; that reload starts a page like any other, which sets JavaScript back.
            if (BrowserConfig.retriesOverHttp(uri)) session.settings.allowJavascript = true
            return GeckoResult.fromValue(errorPage(uri, error))
        }
    }

    private val progressListener = object : GeckoSession.ProgressDelegate {
        override fun onPageStart(session: GeckoSession, url: String) {
            val tab = tabOf(session) ?: return
            // Again for loads that do not ask first, such as reloads: still before the page arrives.
            val uri = Uri.parse(url)
            if (BrowserConfig.isWebPage(uri.scheme, uri.host)) allowScripts(session, url)
            tab.loading = true
            tab.progress = 0
            if (tab !== shown) return
            setLoading(true)
            progress.progress = 0
        }

        override fun onPageStop(session: GeckoSession, success: Boolean) {
            val tab = tabOf(session) ?: return
            tab.loading = false
            if (tab === shown) setLoading(false)
        }

        override fun onProgressChange(session: GeckoSession, progress: Int) {
            val tab = tabOf(session) ?: return
            tab.progress = progress
            if (tab === shown) this@BrowserActivity.progress.progress = progress
        }

        override fun onSecurityChange(session: GeckoSession, securityInfo: GeckoSession.ProgressDelegate.SecurityInformation) {
            val tab = tabOf(session) ?: return
            tab.secure = securityInfo.isSecure
            if (tab === shown) showKind(tab.url, tab.secure)
        }
    }

    private val content = object : GeckoSession.ContentDelegate {
        override fun onTitleChange(session: GeckoSession, title: String?) {
            tabOf(session)?.title = title
            if (tabList.visibility == View.VISIBLE) tabRows.notifyDataSetChanged()
        }

        // window.close() from a page a script opened.
        override fun onCloseRequest(session: GeckoSession) {
            val tab = tabOf(session) ?: return
            closeTab(tab, then = tab.opener)
        }

        override fun onContextMenu(session: GeckoSession, screenX: Int, screenY: Int, element: ContextElement) {
            showLinkMenu(tabOf(session) ?: return, element)
        }

        override fun onExternalResponse(session: GeckoSession, response: WebResponse) {
            Toast.makeText(this@BrowserActivity, R.string.browser_no_downloads, Toast.LENGTH_LONG).show()
        }

        override fun onCrash(session: GeckoSession) = recover(session)

        override fun onKill(session: GeckoSession) = recover(session)
    }

    /** A tab's page process died: a new session in its place, back where it was. */
    private fun recover(session: GeckoSession) {
        val ready = runtime ?: return
        val tab = tabOf(session) ?: return
        val onScreen = geckoView.session === session
        if (onScreen) geckoView.releaseSession()
        session.close()
        tab.session = newSession(tab.url).also { it.open(ready) }
        if (onScreen) geckoView.setSession(tab.session) else tab.session.setActive(false)
        tab.url?.let { load(tab, it) }
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

    /** What a long press on a link or an image offers. Nothing is ever saved to the phone. */
    private fun showLinkMenu(tab: Tab, element: ContextElement) {
        fun web(url: String?) = url?.takeIf { Uri.parse(it).let { u -> BrowserConfig.isWebPage(u.scheme, u.host) } }
        val link = web(element.linkUri)
        val image = web(element.srcUri)?.takeIf { element.type == ContextElement.TYPE_IMAGE }
        val actions = mutableListOf<Pair<Int, () -> Unit>>()
        if (link != null) {
            actions += R.string.browser_open_new_tab to { openTab(link, opener = tab) }
            actions += R.string.browser_open_background_tab to { openTab(link, opener = tab, background = true) }
            actions += R.string.browser_copy_link to { copy(BrowserConfig.shareable(link)) }
        }
        if (image != null) {
            actions += R.string.browser_open_image to { openTab(image, opener = tab) }
            actions += R.string.browser_copy_image_address to { copy(BrowserConfig.shareable(image)) }
        }
        if (actions.isEmpty()) return
        AlertDialog.Builder(this)
            .setTitle(BrowserConfig.displayAddress(link ?: checkNotNull(image)))
            .setItems(actions.map { getString(it.first) }.toTypedArray()) { _, which -> actions[which].second() }
            .show()
    }

    private fun copy(text: String) {
        val clip = ClipData.newPlainText(null, text)
        // Kept out of the clipboard's preview, as a password is: it is where someone has been.
        clip.description.extras = PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
        getSystemService(ClipboardManager::class.java)?.setPrimaryClip(clip)
        // From Android 13 the system says so itself.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) Toast.makeText(this, R.string.browser_copied, Toast.LENGTH_SHORT).show()
    }

    /**
     * Closes every tab and its data, and gives every site new Tor circuits: nothing seen so far
     * can link what comes next.
     */
    private fun newIdentity() {
        val ready = runtime ?: return
        geckoView.releaseSession()
        tabs.all.forEach { it.session.close() }
        tabs.clear()
        render()
        ready.storageController.clearData(StorageController.ClearFlags.ALL)
        Thread {
            Native.newIdentity()
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                openTab(null)
                Toast.makeText(this, R.string.browser_new_identity_done, Toast.LENGTH_SHORT).show()
            }
        }.start()
    }

    private fun confirmNewIdentity() {
        AlertDialog.Builder(this)
            .setTitle(R.string.browser_new_identity)
            .setMessage(R.string.browser_new_identity_confirm)
            .setPositiveButton(R.string.browser_new_identity) { _, _ -> newIdentity() }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun askSecurityLevel() {
        val levels = SecurityLevel.entries
        // Each level's name, and under it, smaller, what it turns off.
        val items = levels.map { option ->
            val about = getString(
                when (option) {
                    SecurityLevel.STANDARD -> R.string.browser_level_standard_about
                    SecurityLevel.SAFER -> R.string.browser_level_safer_about
                    SecurityLevel.SAFEST -> R.string.browser_level_safest_about
                },
            )
            SpannableStringBuilder().apply {
                append(levelName(option), StyleSpan(Typeface.BOLD), Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                append("\n")
                val start = length
                append(about)
                setSpan(ForegroundColorSpan(getColor(R.color.muted)), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                setSpan(RelativeSizeSpan(0.87f), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        AlertDialog.Builder(this)
            .setTitle(R.string.browser_security_level)
            .setSingleChoiceItems(items.toTypedArray<CharSequence>(), levels.indexOf(level)) { dialog, which ->
                dialog.dismiss()
                setLevel(levels[which])
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun levelName(option: SecurityLevel) = getString(
        when (option) {
            SecurityLevel.STANDARD -> R.string.browser_level_standard
            SecurityLevel.SAFER -> R.string.browser_level_safer
            SecurityLevel.SAFEST -> R.string.browser_level_safest
        },
    )

    /** Takes [chosen] into Gecko, then loads every open page again under it. */
    private fun setLevel(chosen: SecurityLevel) {
        if (chosen == level) return
        Browser.apply(chosen).accept(
            {
                level = chosen
                prefs().edit().putString(SecurityLevel.KEY, chosen.name).apply()
                for (tab in tabs.all) {
                    allowScripts(tab.session, tab.url)
                    if (tab.url != null) tab.session.reload()
                }
            },
            { error -> Toast.makeText(this, getString(R.string.browser_level_failed, error?.message ?: error.toString()), Toast.LENGTH_LONG).show() },
        )
    }

    private fun bookmarks() = Bookmarks.parse(prefs().getString(Bookmarks.KEY, null))

    private fun saveBookmarks(list: List<Bookmark>) = prefs().edit().putString(Bookmarks.KEY, Bookmarks.serialize(list)).apply()

    private fun toggleBookmark(tab: Tab) {
        val url = tab.url ?: return
        val before = bookmarks()
        val after = Bookmarks.toggle(before, Bookmark(url, tabName(tab)))
        saveBookmarks(after)
        Toast.makeText(this, if (after.size > before.size) R.string.browser_bookmarked else R.string.browser_bookmark_removed, Toast.LENGTH_SHORT).show()
    }

    /** The bookmarks, newest first: a tap opens one in the shown tab. */
    private fun showBookmarks() {
        val list = bookmarks().toMutableList()
        if (list.isEmpty()) {
            AlertDialog.Builder(this)
                .setTitle(R.string.browser_bookmarks)
                .setMessage(R.string.browser_bookmarks_empty)
                .setPositiveButton(android.R.string.ok, null)
                .show()
            return
        }
        lateinit var dialog: AlertDialog
        val rows = object : BaseAdapter() {
            override fun getCount() = list.size
            override fun getItem(position: Int) = list[position]
            override fun getItemId(position: Int) = position.toLong()
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val mark = list[position]
                return row(
                    mark.title.ifBlank { BrowserConfig.displayAddress(mark.url) },
                    mark.url,
                    secure = null,
                    current = false,
                    removeLabel = getString(R.string.browser_remove_bookmark),
                    open = {
                        dialog.dismiss()
                        shown?.let { load(it, mark.url) } ?: openTab(mark.url)
                    },
                    remove = {
                        list.remove(mark)
                        saveBookmarks(list)
                        notifyDataSetChanged()
                    },
                )
            }
        }
        dialog = AlertDialog.Builder(this)
            .setTitle(R.string.browser_bookmarks)
            .setView(ListView(this).apply {
                adapter = rows
                divider = getDrawable(R.drawable.rule_dashed)
                dividerHeight = dp(3)
            })
            .setNegativeButton(R.string.browser_close_list, null)
            .show()
    }

    private fun tabName(tab: Tab): String =
        tab.title?.takeIf { it.isNotBlank() } ?: tab.url?.let(BrowserConfig::displayAddress) ?: getString(R.string.browser_new_tab)

    private fun setLoading(loading: Boolean) {
        progress.visibility = if (loading) View.VISIBLE else View.INVISIBLE
        reload.setImageResource(if (loading) R.drawable.ic_stop else R.drawable.ic_reload)
        reload.contentDescription = getString(if (loading) R.string.browser_stop else R.string.browser_reload)
    }

    private fun setEnabled(button: ImageButton, enabled: Boolean) {
        button.isEnabled = enabled
        button.imageAlpha = if (enabled) 255 else 80
    }

    private class Kind(val label: String, val color: Int, val sealed: Boolean)

    /** Which network a page came over, and whether the connection to it is encrypted. */
    private fun kindOf(url: String?, secure: Boolean?): Kind {
        val uri = url?.let(Uri::parse)
        val host = uri?.host.orEmpty()
        return when {
            uri == null || !BrowserConfig.isWebPage(uri.scheme, host) -> Kind("", R.color.muted, true)
            host.endsWith(".onion") -> Kind(getString(R.string.browser_kind_onion), R.color.onion, true)
            host.endsWith(".i2p") -> Kind("I2P", R.color.i2p, true)
            host == BrowserConfig.HYPHANET_HOST -> Kind("Hyphanet", R.color.hyphanet, true)
            secure == false || uri.scheme == "http" -> Kind(getString(R.string.browser_tor_plain), R.color.warn, false)
            else -> Kind("Tor", R.color.ok, true)
        }
    }

    private fun showKind(url: String?, secure: Boolean?) = showBadge(kind, kindOf(url, secure))

    /** A network badge, as the search page's route badges: the kind in capitals, a lock, an outline. */
    private fun badge() = TextView(this).apply {
        gravity = Gravity.CENTER_VERTICAL
        includeFontPadding = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
        typeface = monoBold
        isAllCaps = true
        letterSpacing = 0.06f
        compoundDrawablePadding = dp(4)
        setPadding(dp(4), dp(4), dp(5), dp(4))
        visibility = View.GONE
    }

    /** Puts [shownKind] on [view] in its colour, or [color] (on the ink block, the ground's). */
    private fun showBadge(view: TextView, shownKind: Kind, color: Int = getColor(shownKind.color)) {
        view.text = shownKind.label
        view.setTextColor(color)
        view.background = GradientDrawable().apply { setStroke(dp(1), color) }
        val lock = checkNotNull(getDrawable(if (shownKind.sealed) R.drawable.ic_lock else R.drawable.ic_lock_open)).mutate()
        lock.setBounds(0, 0, dp(12), dp(12))
        lock.setTint(color)
        view.setCompoundDrawablesRelative(lock, null, null, null)
        view.visibility = if (shownKind.label.isEmpty()) View.GONE else View.VISIBLE
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

    private fun hideKeyboard(from: View) {
        getSystemService(InputMethodManager::class.java)?.hideSoftInputFromWindow(from.windowToken, 0)
    }

    private fun go() {
        val target = BrowserConfig.address(address.text.toString())
        if (target == null) {
            Toast.makeText(this, R.string.browser_not_an_address, Toast.LENGTH_SHORT).show()
            return
        }
        // Focus to the page, so the address bar shows where the page went.
        geckoView.requestFocus()
        hideKeyboard(address)
        shown?.let { load(it, target) } ?: openTab(target)
    }

    private fun openElsewhere(uri: Uri) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
        } catch (_: ActivityNotFoundException) {
            Toast.makeText(this, uri.toString(), Toast.LENGTH_LONG).show()
        }
    }

    private fun showTabList(show: Boolean) {
        if (show) {
            if (findBar.visibility == View.VISIBLE) closeFind()
            hideKeyboard(address)
        }
        tabList.visibility = if (show) View.VISIBLE else View.GONE
        bar.visibility = if (show) View.GONE else View.VISIBLE
        geckoView.visibility = if (show || message.visibility == View.VISIBLE) View.GONE else View.VISIBLE
        render()
        if (show) progress.visibility = View.INVISIBLE
    }

    private fun openFind() {
        val tab = shown ?: return
        tab.session.finder.displayFlags = GeckoSession.FINDER_DISPLAY_HIGHLIGHT_ALL
        bar.visibility = View.GONE
        findBar.visibility = View.VISIBLE
        findText.setText("")
        findCount.text = ""
        findText.requestFocus()
        getSystemService(InputMethodManager::class.java)?.showSoftInput(findText, InputMethodManager.SHOW_IMPLICIT)
    }

    private fun closeFind() {
        shown?.session?.finder?.clear()
        findBar.visibility = View.GONE
        bar.visibility = View.VISIBLE
        hideKeyboard(findText)
    }

    /** Finds what is typed: from the top when it changes, else the next match, or the one before. */
    private fun find(again: Boolean, backwards: Boolean = false, recount: Boolean = true) {
        val finder = shown?.session?.finder ?: return
        val text = findText.text.toString()
        if (text.isEmpty()) {
            finder.clear()
            findCount.text = ""
            return
        }
        val asked = ++finds
        finder.find(if (again) null else text, if (backwards) GeckoSession.FINDER_FIND_BACKWARDS else 0).accept { result ->
            if (asked != finds) return@accept
            when {
                result?.found != true -> findCount.text = getString(R.string.browser_find_none)
                result.total > 0 -> findCount.text = getString(R.string.browser_find_count, result.current, result.total)
                // Found before Gecko had counted the matches: ask once more, which finds the same
                // match again (a new search starts at the selection) and has the count by then.
                recount -> findCount.postDelayed({ if (asked == finds) find(again = false, recount = false) }, 250)
                else -> findCount.text = ""
            }
        }
    }

    private fun showMenu(anchor: View) {
        val tab = shown
        val page = tab?.url?.takeIf { Uri.parse(it).let { u -> BrowserConfig.isWebPage(u.scheme, u.host) } }
        val marked = page != null && bookmarks().any { it.url == page }
        PopupMenu(this, anchor).apply {
            menu.add(0, MENU_FORWARD, 0, R.string.browser_forward).isEnabled = tab?.canGoForward == true
            menu.add(0, MENU_NEW_TAB, 1, R.string.browser_new_tab)
            menu.add(0, MENU_FIND, 2, R.string.browser_find).isEnabled = page != null
            menu.add(0, MENU_BOOKMARK, 3, if (marked) R.string.browser_unbookmark else R.string.browser_bookmark).isEnabled = page != null
            menu.add(0, MENU_BOOKMARKS, 4, R.string.browser_bookmarks)
            menu.add(0, MENU_COPY, 5, R.string.browser_copy_link).isEnabled = page != null
            menu.add(0, MENU_LEVEL, 6, getString(R.string.browser_security_level_now, levelName(level)))
            menu.add(0, MENU_IDENTITY, 7, R.string.browser_new_identity)
            menu.add(0, MENU_SCREENSHOTS, 8, R.string.browser_allow_screenshots).apply {
                isCheckable = true
                isChecked = screenshotsAllowed()
            }
            menu.add(0, MENU_CLOSE, 9, R.string.browser_close)
            setOnMenuItemClickListener { item ->
                when (item.itemId) {
                    MENU_FORWARD -> tab?.session?.goForward()
                    MENU_NEW_TAB -> openTab(null)
                    MENU_FIND -> openFind()
                    MENU_BOOKMARK -> tab?.let(::toggleBookmark)
                    MENU_BOOKMARKS -> showBookmarks()
                    MENU_COPY -> page?.let { copy(BrowserConfig.shareable(it)) }
                    MENU_LEVEL -> askSecurityLevel()
                    MENU_IDENTITY -> confirmNewIdentity()
                    MENU_SCREENSHOTS -> {
                        prefs().edit().putBoolean(KEY_SCREENSHOTS, !item.isChecked).apply()
                        applyScreenshotSetting()
                    }
                    MENU_CLOSE -> finishAndRemoveTask()
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

    private fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()

    private fun ripple(borderless: Boolean) = TypedValue().also {
        theme.resolveAttribute(if (borderless) android.R.attr.selectableItemBackgroundBorderless else android.R.attr.selectableItemBackground, it, true)
    }.resourceId

    private fun button(icon: Int, description: Int, onClick: (View) -> Unit) = ImageButton(this).apply {
        setImageResource(icon)
        setBackgroundResource(ripple(borderless = true))
        contentDescription = getString(description)
        setOnClickListener(onClick)
    }

    /** A text field on the address rule, in the mono. */
    private fun field(hint: Int) = EditText(this).apply {
        this.hint = getString(hint)
        isSingleLine = true
        ellipsize = TextUtils.TruncateAt.END
        // The small step: the mono is wide, and addresses are long. What does not fit fades out at
        // the edge, since an editable field cannot end in an ellipsis.
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
        typeface = mono
        isHorizontalFadingEdgeEnabled = true
        setFadingEdgeLength(dp(20))
        setTextColor(getColor(R.color.ink))
        setHintTextColor(getColor(R.color.muted))
        background = null
        setPadding(0, 0, 0, 0)
    }

    private fun pill() = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        // Centre the badge and the text on each other, not on their text baselines.
        isBaselineAligned = false
        setBackgroundResource(R.drawable.bg_address)
        setPadding(dp(4), 0, 0, 0)
    }

    private fun toolbar(vararg views: Pair<View, LinearLayout.LayoutParams>) = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        setBackgroundColor(getColor(R.color.ground))
        setPadding(dp(2), dp(6), dp(2), dp(6))
        for ((view, params) in views) addView(view, params)
    }

    private fun square() = LinearLayout.LayoutParams(dp(48), dp(48))

    private fun buildViews() {
        back = button(R.drawable.ic_back, R.string.browser_back) { shown?.session?.goBack() }
        reload = button(R.drawable.ic_reload, R.string.browser_reload) {
            val tab = shown ?: return@button
            if (tab.loading) tab.session.stop() else tab.session.reload()
        }
        tabButton = TabButton(this).apply { setOnClickListener { showTabList(true) } }
        val menu = button(R.drawable.ic_more, R.string.browser_menu, ::showMenu)
        setEnabled(back, false)
        kind = badge()
        address = field(R.string.browser_address_hint).apply {
            inputType = InputType.TYPE_TEXT_VARIATION_URI or InputType.TYPE_CLASS_TEXT
            // The keyboard must not learn or sync what is typed here.
            imeOptions = EditorInfo.IME_ACTION_GO or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            // The full address to edit, selected; the short form again when done. Without
            // select-all-on-focus the tap that focuses the field drops the cursor where it landed.
            setSelectAllOnFocus(true)
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
        // The address pill: where the page came from and how, the address, and reload.
        val addressPill = pill().apply {
            addView(kind, LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginEnd = dp(6) })
            addView(address, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            addView(reload, LinearLayout.LayoutParams(dp(48), dp(48)))
            setOnClickListener { focusAddress() }
        }
        bar = toolbar(
            back to square(),
            addressPill to LinearLayout.LayoutParams(0, dp(48), 1f).apply { marginStart = dp(2); marginEnd = dp(2) },
            tabButton to square(),
            menu to square(),
        )

        findText = field(R.string.browser_find).apply {
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            imeOptions = EditorInfo.IME_ACTION_SEARCH or EditorInfo.IME_FLAG_NO_PERSONALIZED_LEARNING
            addTextChangedListener(object : TextWatcher {
                override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
                override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
                override fun afterTextChanged(s: Editable?) {
                    findText.removeCallbacks(findTyped)
                    findText.postDelayed(findTyped, 150)
                }
            })
            setOnEditorActionListener { _, _, _ ->
                find(again = true)
                true
            }
        }
        findCount = TextView(this).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
            typeface = mono
            setTextColor(getColor(R.color.muted))
            setPadding(dp(8), 0, dp(10), 0)
        }
        val findPill = pill().apply {
            addView(findText, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            addView(findCount)
        }
        findBar = toolbar(
            findPill to LinearLayout.LayoutParams(0, dp(48), 1f).apply { marginStart = dp(6); marginEnd = dp(2) },
            button(R.drawable.ic_up, R.string.browser_find_previous) { find(again = true, backwards = true) } to square(),
            button(R.drawable.ic_down, R.string.browser_find_next) { find(again = true) } to square(),
            button(R.drawable.ic_close, R.string.browser_find_close) { closeFind() } to square(),
        ).apply { visibility = View.GONE }

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
        tabList = buildTabList().apply { visibility = View.GONE }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(getColor(R.color.ground))
            addView(bar, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(findBar, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(progress, LinearLayout.LayoutParams.MATCH_PARENT, dp(2))
            addView(message, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(geckoView, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
            addView(tabList, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
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

    /** Every open tab, the shown one marked: a tap shows one, × closes it. */
    private fun buildTabList(): View {
        tabListTitle = TextView(this).apply {
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            typeface = monoBold
            isAllCaps = true
            letterSpacing = 0.04f
            setTextColor(getColor(R.color.ink))
            setPadding(dp(14), 0, 0, 0)
        }
        val header = toolbar(
            tabListTitle to LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f),
            button(R.drawable.ic_add, R.string.browser_new_tab) { openTab(null) } to square(),
        )
        tabRows = object : BaseAdapter() {
            override fun getCount() = tabs.size
            override fun getItem(position: Int) = tabs.all[position]
            override fun getItemId(position: Int) = position.toLong()
            override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
                val tab = tabs.all[position]
                return row(
                    tabName(tab),
                    tab.url,
                    tab.secure,
                    current = position == tabs.current,
                    removeLabel = getString(R.string.browser_close_tab),
                    open = { showTab(tab) },
                    remove = { closeTab(tab) },
                )
            }
        }
        val rows = ListView(this).apply {
            adapter = tabRows
            divider = getDrawable(R.drawable.rule_dashed)
            dividerHeight = dp(3)
            clipToPadding = false
            setPadding(0, 0, 0, dp(8))
        }
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(header, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(rows, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f))
        }
    }

    /**
     * A row of the tab list or the bookmarks: the name, then its network's badge and short address,
     * and a button to close or remove it.
     */
    private fun row(
        title: String,
        url: String?,
        secure: Boolean?,
        current: Boolean,
        removeLabel: String,
        open: () -> Unit,
        remove: () -> Unit,
    ): View {
        // On the ink block everything takes the ground's colour, the network's included.
        val ink = getColor(if (current) R.color.ground else R.color.ink)
        val text = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(TextView(this@BrowserActivity).apply {
                text = title
                isSingleLine = true
                ellipsize = TextUtils.TruncateAt.END
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
                typeface = monoBold
                letterSpacing = -0.02f
                setTextColor(ink)
            })
            if (url != null) {
                val at = kindOf(url, secure)
                addView(LinearLayout(this@BrowserActivity).apply {
                    orientation = LinearLayout.HORIZONTAL
                    gravity = Gravity.CENTER_VERTICAL
                    isBaselineAligned = false
                    addView(badge().also { showBadge(it, at, if (current) ink else getColor(at.color)) },
                        LinearLayout.LayoutParams(LinearLayout.LayoutParams.WRAP_CONTENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { marginEnd = dp(8) })
                    addView(TextView(this@BrowserActivity).apply {
                        text = BrowserConfig.displayAddress(url)
                        isSingleLine = true
                        ellipsize = TextUtils.TruncateAt.END
                        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
                        typeface = mono
                        setTextColor(if (current) ink else getColor(R.color.muted))
                    }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
                }, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT).apply { topMargin = dp(4) })
            }
        }
        val inner = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = dp(60)
            setPadding(dp(12), dp(6), dp(2), dp(6))
            if (current) setBackgroundResource(R.drawable.bg_tab_current) else setBackgroundResource(ripple(borderless = false))
            addView(text, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            addView(ImageButton(this@BrowserActivity).apply {
                setImageResource(R.drawable.ic_close)
                setColorFilter(ink)
                setBackgroundResource(ripple(borderless = true))
                contentDescription = "$removeLabel: $title"
                // A focusable child would take the row's taps in a ListView.
                isFocusable = false
                setOnClickListener { remove() }
            }, square())
            setOnClickListener { open() }
        }
        return FrameLayout(this).apply {
            setPadding(dp(8), dp(2), dp(8), dp(2))
            addView(inner)
        }
    }

    /** A page saying why a load failed, as a data: URL Gecko shows in place of the page. */
    private fun errorPage(uri: String?, error: WebRequestError): String {
        val reason = getString(
            when {
                // omnid's proxy refuses a .i2p site when no I2P router answers.
                error.code == WebRequestError.ERROR_CONNECTION_REFUSED && BrowserConfig.isI2p(uri) -> R.string.browser_error_no_i2p_router
                // And a Hyphanet page when no node answers.
                error.code == WebRequestError.ERROR_CONNECTION_REFUSED && BrowserConfig.isHyphanet(uri) -> R.string.browser_error_no_hyphanet_node
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
        val font = errorFont ?: ("data:font/woff;base64," +
            Base64.encodeToString(assets.open("fonts/space-mono-bold.woff").use { it.readBytes() }, Base64.NO_WRAP)).also { errorFont = it }
        // The brand's dark scheme always: Gecko's fingerprinting protection reports a light one.
        val html = "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'>" +
            "<style>@font-face{font-family:m;font-weight:700;src:url($font)}</style>" +
            "<body style='font:15px/1.55 sans-serif;margin:0;padding:3rem 1.5rem;background:#0B0D1F;color:#B2BBDF;min-height:100vh'>" +
            "<h1 style='margin:0 0 .5rem;font:700 21.6px/1.3 m,monospace;letter-spacing:-.04em;color:#EEF2FF'>${escape(getString(R.string.browser_error_title))}</h1>" +
            "<p style='margin:0 0 1.25rem'>${escape(reason)}</p>" +
            "<p style='margin:0;word-break:break-all;color:#8690BD;font:12.5px/1.5 monospace'>${escape(uri.orEmpty())}</p>" +
            "<p style='margin:.5rem 0 0;color:#8690BD;font-size:12.5px'>${escape(getString(R.string.browser_error_code, "0x" + Integer.toHexString(error.code)))}</p>" +
            // A .i2p site or Hyphanet page over HTTPS: reload it over HTTP with an exception for
            // this site, before the error is drawn. Gecko gives error pages that call; without it the error shows.
            (if (BrowserConfig.retriesOverHttp(uri)) "<script>if(document.reloadWithHttpsOnlyException){document.body.hidden=true;document.reloadWithHttpsOnlyException()}</script>" else "")
        return "data:text/html;charset=utf-8," + Uri.encode(html)
    }
}

/** How many tabs are open, in a frame: the way to the tab list, in the browser and on the search page. */
internal class TabButton(context: Context) : FrameLayout(context) {
    private val number = TextView(context).apply {
        gravity = Gravity.CENTER
        includeFontPadding = false
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
        typeface = context.resources.getFont(R.font.space_mono_bold)
        setTextColor(context.getColor(R.color.ink))
        setBackgroundResource(R.drawable.bg_tab_count)
    }

    var count = 0
        set(value) {
            field = value
            number.text = value.toString()
            contentDescription = context.getString(R.string.browser_tabs_open, value)
        }

    init {
        val density = resources.displayMetrics.density
        val ripple = TypedValue().also { context.theme.resolveAttribute(android.R.attr.selectableItemBackgroundBorderless, it, true) }.resourceId
        setBackgroundResource(ripple)
        minimumWidth = (48 * density).toInt()
        minimumHeight = (48 * density).toInt()
        addView(number, LayoutParams((22 * density).toInt(), (22 * density).toInt(), Gravity.CENTER))
        count = 0
    }
}
