package cc.wckd.omnid

import android.app.Activity
import android.app.AlertDialog
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.os.PersistableBundle
import android.text.InputType
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.text.style.StyleSpan
import android.util.TypedValue
import android.view.Gravity
import android.view.MenuItem
import android.view.View
import android.view.WindowInsets
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import java.io.File

/**
 * OMNId's settings, one screen: appearance, the browser, search, crawling, the routers elsewhere,
 * and about. Changes save at once. Browser settings reach Gecko when the browser comes back to the
 * front; settings the daemon reads at start (crawl speed, routers, blocked words and sites)
 * restart it.
 */
class SettingsActivity : Activity() {
    private lateinit var content: LinearLayout
    private lateinit var drawnWith: Settings.Theme

    private val mono by lazy { resources.getFont(R.font.space_mono) }
    private val monoBold by lazy { resources.getFont(R.font.space_mono_bold) }

    override fun attachBaseContext(base: Context) {
        super.attachBaseContext(base)
        Appearance.apply(this, base)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        drawnWith = Appearance.chosen(this)
        actionBar?.apply {
            setDisplayHomeAsUpEnabled(true)
            setDisplayShowTitleEnabled(false)
            setDisplayShowCustomEnabled(true)
            customView = TextView(this@SettingsActivity).apply {
                text = getString(R.string.settings)
                setTextColor(getColor(R.color.ink))
                setTextSize(TypedValue.COMPLEX_UNIT_SP, 18f)
                typeface = monoBold
                letterSpacing = -0.02f
            }
        }
        content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(0, 0, 0, dp(32))
        }
        val scroll = ScrollView(this).apply {
            addView(content)
            // Rows scroll on under the bars; at rest the list starts below them.
            clipToPadding = false
            // Edge to edge from Android 15: keep the list clear of the system bars and the action
            // bar, which the top inset includes.
            setOnApplyWindowInsetsListener { view, insets ->
                val bars = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                    insets.getInsets(WindowInsets.Type.systemBars())
                } else {
                    @Suppress("DEPRECATION")
                    insets.systemWindowInsets
                }
                view.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                insets
            }
        }
        setContentView(scroll)
        render()
        if (savedInstanceState == null && intent.getBooleanExtra(EXTRA_I2P_ROUTER, false)) askI2pRouter()
    }

    /** The daemon restarting changes what the proxy for other apps shows. */
    private val onServiceChange: () -> Unit = { runOnUiThread(::render) }

    override fun onStart() {
        super.onStart()
        OmnidService.observe(onServiceChange)
    }

    override fun onStop() {
        OmnidService.unobserve(onServiceChange)
        super.onStop()
    }

    override fun onResume() {
        super.onResume()
        Appearance.applyScreenshots(this)
        if (Appearance.chosen(this) != drawnWith) recreate()
    }

    override fun onOptionsItemSelected(item: MenuItem): Boolean {
        if (item.itemId == android.R.id.home) {
            finish()
            return true
        }
        return super.onOptionsItemSelected(item)
    }

    private fun prefs() = getSharedPreferences(CrawlPolicy.PREFS, MODE_PRIVATE)

    /** Draws every row again from the stored settings, so each shows its value. */
    private fun render() {
        content.removeAllViews()
        val prefs = prefs()

        section(R.string.settings_appearance)
        choice(R.string.settings_theme, themeName(Settings.theme(prefs.getString(Settings.THEME, null)))) { askTheme() }
        divider()
        choice(R.string.settings_text_size, textSizeName(Settings.textSize(prefs.getFloat(Settings.TEXT_SIZE, 1f)))) { askTextSize() }

        section(R.string.settings_browser)
        choice(R.string.browser_security_level, levelName(SecurityLevel.from(prefs.getString(SecurityLevel.KEY, null)))) { askLevel() }
        divider()
        toggle(R.string.settings_images, R.string.settings_images_about, Settings.IMAGES, default = true)
        divider()
        toggle(R.string.browser_allow_screenshots, R.string.settings_screenshots_about, Settings.SCREENSHOTS, default = false) {
            Appearance.applyScreenshots(this)
        }
        divider()
        choice(R.string.settings_download_connections, connectionsName(Downloads.connections(prefs.getInt(Downloads.CONNECTIONS, Downloads.DEFAULT_CONNECTIONS)))) {
            askConnections()
        }

        section(R.string.settings_search)
        choice(R.string.settings_networks, networksName(Settings.networks(prefs.getString(Settings.SEARCH_NETWORKS, null)))) { askNetworks() }
        divider()
        val languages = prefs.getString(Settings.SEARCH_LANGUAGES, "").orEmpty()
        choice(R.string.settings_languages, languages.ifEmpty { getString(R.string.settings_languages_phone) }.replace(",", ", ")) { askLanguages() }
        divider()
        choice(R.string.settings_blocked_words, listName(Settings.BLOCKED_WORDS_FILE)) {
            askList(R.string.settings_blocked_words, R.string.settings_blocked_words_detail, Settings.BLOCKED_WORDS_FILE)
        }
        divider()
        choice(R.string.settings_blocked_sites, listName(Settings.BLOCKED_SITES_FILE)) {
            askList(R.string.settings_blocked_sites, R.string.settings_blocked_sites_detail, Settings.BLOCKED_SITES_FILE)
        }

        section(R.string.settings_crawling)
        choice(R.string.settings_crawl_speed, speedName(Settings.crawlSpeed(prefs.getString(Settings.CRAWL_SPEED, null)))) { askSpeed() }
        divider()
        toggle(R.string.crawl_anytime, R.string.settings_anytime_about, CrawlPolicy.KEY_ANYTIME, default = false)
        divider()
        toggle(R.string.settings_blocklist, R.string.settings_blocklist_about, Settings.ABUSE_BLOCKLIST, default = true) { restartDaemon() }

        section(R.string.settings_anonymity)
        toggle(R.string.settings_crawl_via_tor, R.string.settings_crawl_via_tor_about, Settings.CRAWL_VIA_TOR, default = true) { restartDaemon() }
        divider()
        toggle(R.string.settings_torrent_lookups, R.string.settings_torrent_lookups_about, Settings.TORRENT_LOOKUPS, default = false) { restartDaemon() }

        section(R.string.settings_routers)
        choice(R.string.i2p_router, prefs.getString(I2pRouter.KEY, "").orEmpty().ifEmpty { getString(R.string.settings_router_phone) }) {
            askI2pRouter()
        }
        divider()
        choice(R.string.hyphanet_node, prefs.getString(HyphanetNode.KEY, "").orEmpty().ifEmpty { getString(R.string.settings_router_phone) }) {
            askRouter(R.string.hyphanet_node, R.string.hyphanet_node_detail, R.string.hyphanet_node_hint, R.string.hyphanet_node_invalid, HyphanetNode.KEY, HyphanetNode::parse)
        }

        section(R.string.settings_app_proxy)
        toggle(R.string.settings_app_proxy_on, R.string.settings_app_proxy_about, Settings.APP_PROXY, default = false) { restartDaemon() }
        if (prefs.getBoolean(Settings.APP_PROXY, false)) {
            val address = "127.0.0.1:${Settings.APP_PROXY_PORT}"
            val endpoint = OmnidService.endpoint
            val state = when {
                endpoint == null -> getString(R.string.settings_app_proxy_starting)
                endpoint.appProxyPort == 0 -> getString(R.string.settings_app_proxy_taken, Settings.APP_PROXY_PORT)
                else -> getString(R.string.settings_app_proxy_running, address)
            }
            divider()
            choice(R.string.settings_app_proxy_address, state) { copy(address, sensitive = false) }
            divider()
            toggle(R.string.settings_app_proxy_locked, R.string.settings_app_proxy_locked_about, Settings.APP_PROXY_LOCKED, default = true) { restartDaemon() }
            if (prefs.getBoolean(Settings.APP_PROXY_LOCKED, true)) {
                divider()
                choice(R.string.settings_app_proxy_password, Settings.appProxyPassword(prefs)) { askProxyPassword() }
            }
        }

        section(R.string.settings_about)
        choice(R.string.settings_version, getString(R.string.settings_version_value, packageManager.getPackageInfo(packageName, 0).versionName)) {}
        divider()
        choice(R.string.settings_source, SOURCE.substringAfter("://")) {
            startActivity(Intent(this, BrowserActivity::class.java).putExtra(BrowserActivity.EXTRA_URL, SOURCE))
        }
        divider()
        choice(R.string.settings_licenses, getString(R.string.settings_licenses_value)) { showLicenses() }
    }

    // Rows.

    /** A section's name in Space Mono capitals over a solid rule, as the search page heads its lists. */
    private fun section(title: Int) {
        content.addView(TextView(this).apply {
            text = getString(title).uppercase()
            typeface = monoBold
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 11f)
            letterSpacing = 0.08f
            setTextColor(getColor(R.color.ink_2))
            setPadding(dp(16), dp(28), dp(16), dp(8))
        })
        content.addView(View(this).apply { setBackgroundColor(getColor(R.color.ink)) }, matchWidth(dp(2)).apply {
            marginStart = dp(16)
            marginEnd = dp(16)
        })
    }

    /** The dashed rule between rows, as between results. */
    private fun divider() {
        content.addView(View(this).apply { setBackgroundResource(R.drawable.rule_dashed) }, matchWidth(dp(3)).apply {
            marginStart = dp(16)
            marginEnd = dp(16)
        })
    }

    private fun matchWidth(height: Int) = LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, height)

    private fun rowTitle(title: Int) = TextView(this).apply {
        text = getString(title)
        setTextColor(getColor(R.color.ink))
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
    }

    private fun rowValue(value: CharSequence) = TextView(this).apply {
        text = value
        typeface = mono
        setTextColor(getColor(R.color.ink_2))
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12.5f)
        setPadding(0, dp(2), 0, 0)
    }

    private fun rowFrame() = LinearLayout(this).apply {
        minimumHeight = dp(56)
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(16), dp(12), dp(16), dp(12))
        background = getDrawable(TypedValue().also { theme.resolveAttribute(android.R.attr.selectableItemBackground, it, true) }.resourceId)
    }

    /** A setting with a value, changed in a dialog. */
    private fun choice(title: Int, value: CharSequence, onClick: () -> Unit) {
        content.addView(rowFrame().apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_VERTICAL or Gravity.START
            addView(rowTitle(title))
            addView(rowValue(value))
            setOnClickListener { onClick() }
        }, matchWidth(LinearLayout.LayoutParams.WRAP_CONTENT))
    }

    /** A setting that is on or off, saved under [key] as it is switched; then [changed] runs. */
    private fun toggle(title: Int, about: Int, key: String, default: Boolean, changed: (() -> Unit)? = null) {
        val switch = Switch(this).apply {
            isChecked = prefs().getBoolean(key, default)
            thumbTintList = ColorStateList(
                arrayOf(intArrayOf(android.R.attr.state_checked), intArrayOf()),
                intArrayOf(getColor(R.color.accent), getColor(R.color.muted)),
            )
            setOnCheckedChangeListener { _, on ->
                prefs().edit().putBoolean(key, on).apply()
                changed?.invoke()
            }
            // The row speaks for it and takes the taps; one control, one name.
            importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
            isClickable = false
            isFocusable = false
        }
        content.addView(rowFrame().apply {
            orientation = LinearLayout.HORIZONTAL
            addView(LinearLayout(this@SettingsActivity).apply {
                orientation = LinearLayout.VERTICAL
                addView(rowTitle(title))
                // A sentence, so the body face; values are in Space Mono.
                addView(TextView(this@SettingsActivity).apply {
                    text = getString(about)
                    setTextColor(getColor(R.color.ink_2))
                    setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
                    setLineSpacing(0f, 1.15f)
                    setPadding(0, dp(2), 0, 0)
                })
            }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f).apply { marginEnd = dp(16) })
            addView(switch)
            contentDescription = getString(title)
            accessibilityDelegate = object : View.AccessibilityDelegate() {
                override fun onInitializeAccessibilityNodeInfo(host: View, info: android.view.accessibility.AccessibilityNodeInfo) {
                    super.onInitializeAccessibilityNodeInfo(host, info)
                    info.isCheckable = true
                    info.isChecked = switch.isChecked
                }
            }
            setOnClickListener { switch.toggle() }
        }, matchWidth(LinearLayout.LayoutParams.WRAP_CONTENT))
    }

    // Values as words.

    private fun themeName(theme: Settings.Theme) = getString(
        when (theme) {
            Settings.Theme.SYSTEM -> R.string.settings_theme_system
            Settings.Theme.LIGHT -> R.string.settings_theme_light
            Settings.Theme.DARK -> R.string.settings_theme_dark
        },
    )

    private fun textSizeName(factor: Float): String {
        val percent = Math.round(factor * 100)
        return if (factor == 1f) getString(R.string.settings_text_size_default, percent) else getString(R.string.settings_text_size_value, percent)
    }

    private fun speedName(speed: Settings.CrawlSpeed) = getString(
        when (speed) {
            Settings.CrawlSpeed.GENTLE -> R.string.settings_speed_gentle
            Settings.CrawlSpeed.NORMAL -> R.string.settings_speed_normal
            Settings.CrawlSpeed.FAST -> R.string.settings_speed_fast
        },
    )

    private fun networkName(network: String) = getString(
        when (network) {
            "onion" -> R.string.network_onion
            "clearnet" -> R.string.network_web
            "magnet" -> R.string.network_torrents
            "i2p" -> R.string.network_i2p
            else -> R.string.network_hyphanet
        },
    )

    private fun networksName(chosen: Set<String>) =
        if (chosen.size == Settings.NETWORKS.size) getString(R.string.settings_networks_all)
        else Settings.NETWORKS.filter { it in chosen }.joinToString(", ") { networkName(it) }

    private fun listFile(name: String) = File(File(filesDir, "omnid"), name)

    private fun listName(name: String): String {
        val entries = runCatching { Settings.entries(listFile(name).readText()) }.getOrDefault(emptyList())
        return if (entries.isEmpty()) getString(R.string.settings_list_none) else resources.getQuantityString(R.plurals.settings_list_count, entries.size, entries.size)
    }

    // Dialogs.

    private fun askTheme() {
        val themes = Settings.Theme.entries
        val current = themes.indexOf(Settings.theme(prefs().getString(Settings.THEME, null)))
        singleChoice(R.string.settings_theme, themes.map { themeName(it) }, current) { which ->
            prefs().edit().putString(Settings.THEME, themes[which].name).apply()
            // This screen first; the others when they come back to the front.
            recreate()
        }
    }

    private fun askTextSize() {
        val current = Settings.TEXT_SIZES.indexOf(Settings.textSize(prefs().getFloat(Settings.TEXT_SIZE, 1f)))
        singleChoice(R.string.settings_text_size, Settings.TEXT_SIZES.map(::textSizeName), current) { which ->
            prefs().edit().putFloat(Settings.TEXT_SIZE, Settings.TEXT_SIZES[which]).apply()
            render()
        }
    }

    private fun askLevel() {
        val levels = SecurityLevel.entries
        val current = levels.indexOf(SecurityLevel.from(prefs().getString(SecurityLevel.KEY, null)))
        singleChoice(R.string.browser_security_level, levelChoices(), current) { which ->
            prefs().edit().putString(SecurityLevel.KEY, levels[which].name).apply()
            render()
        }
    }

    private fun connectionsName(n: Int) =
        if (n == 1) getString(R.string.settings_download_connections_one) else getString(R.string.settings_download_connections_value, n)

    private fun askConnections() {
        val choices = Downloads.CONNECTION_CHOICES
        val current = choices.indexOf(Downloads.connections(prefs().getInt(Downloads.CONNECTIONS, Downloads.DEFAULT_CONNECTIONS)))
        val about = getString(R.string.settings_download_connections_about)
        pickOne(getString(R.string.settings_download_connections), choices.map(::connectionsName), current, about) { which ->
            prefs().edit().putInt(Downloads.CONNECTIONS, choices[which]).apply()
            render()
        }
    }

    private fun askSpeed() {
        val speeds = Settings.CrawlSpeed.entries
        val about = listOf(R.string.settings_speed_gentle_about, R.string.settings_speed_normal_about, R.string.settings_speed_fast_about)
        val items = speeds.mapIndexed { i, speed -> twoLine(speedName(speed), getString(about[i])) }
        val current = speeds.indexOf(Settings.crawlSpeed(prefs().getString(Settings.CRAWL_SPEED, null)))
        singleChoice(R.string.settings_crawl_speed, items, current) { which ->
            if (which != current) {
                prefs().edit().putString(Settings.CRAWL_SPEED, speeds[which].name).apply()
                restartDaemon()
            }
            render()
        }
    }

    private fun askNetworks() {
        val chosen = Settings.networks(prefs().getString(Settings.SEARCH_NETWORKS, null)).toMutableSet()
        val checked = Settings.NETWORKS.map { it in chosen }.toBooleanArray()
        AlertDialog.Builder(this)
            .setTitle(R.string.settings_networks)
            .setMultiChoiceItems(Settings.NETWORKS.map(::networkName).toTypedArray(), checked) { _, which, on ->
                if (on) chosen.add(Settings.NETWORKS[which]) else chosen.remove(Settings.NETWORKS[which])
            }
            .setPositiveButton(R.string.save) { _, _ ->
                if (chosen.isEmpty()) {
                    Toast.makeText(this, R.string.settings_networks_none, Toast.LENGTH_LONG).show()
                } else {
                    prefs().edit().putString(Settings.SEARCH_NETWORKS, Settings.storeNetworks(chosen)).apply()
                    render()
                }
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    private fun askLanguages() {
        val input = textField(prefs().getString(Settings.SEARCH_LANGUAGES, "").orEmpty().replace(",", ", "), R.string.settings_languages_hint)
        AlertDialog.Builder(this)
            .setTitle(R.string.settings_languages)
            .setMessage(R.string.settings_languages_detail)
            .setView(framed(input))
            .setPositiveButton(R.string.save) { _, _ ->
                val languages = Settings.languages(input.text.toString())
                if (languages == null) {
                    Toast.makeText(this, R.string.settings_languages_invalid, Toast.LENGTH_LONG).show()
                } else {
                    prefs().edit().putString(Settings.SEARCH_LANGUAGES, languages).apply()
                    render()
                }
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    /** A list the daemon reads at start, one entry per line, edited whole. */
    private fun askList(title: Int, detail: Int, name: String) {
        val file = listFile(name)
        val current = runCatching { Settings.entries(file.readText()) }.getOrDefault(emptyList())
        val input = EditText(this).apply {
            setText(current.joinToString("\n"))
            typeface = mono
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
            gravity = Gravity.TOP or Gravity.START
            minLines = 5
            maxLines = 10
        }
        AlertDialog.Builder(this)
            .setTitle(title)
            .setMessage(detail)
            .setView(framed(input))
            .setPositiveButton(R.string.save) { _, _ ->
                val entries = Settings.entries(input.text.toString())
                if (entries != current) {
                    file.parentFile?.mkdirs()
                    val written = File(file.parentFile, "$name.new")
                    written.writeText(entries.joinToString("\n", postfix = if (entries.isEmpty()) "" else "\n"))
                    written.renameTo(file)
                    restartDaemon()
                }
                render()
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    /**
     * A router elsewhere that omnid reaches a network through, saved under [key] as [parse] reads
     * it; a change restarts the daemon, which takes it at start.
     */
    private fun askI2pRouter() =
        askRouter(R.string.i2p_router, R.string.i2p_router_detail, R.string.i2p_router_hint, R.string.i2p_router_invalid, I2pRouter.KEY, I2pRouter::parse)

    private fun askRouter(title: Int, detail: Int, hint: Int, invalid: Int, key: String, parse: (String) -> String?) {
        val current = prefs().getString(key, "").orEmpty()
        val input = textField(current, hint).apply { inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI }
        AlertDialog.Builder(this)
            .setTitle(title)
            .setMessage(detail)
            .setView(framed(input))
            .setPositiveButton(R.string.save) { _, _ ->
                val address = parse(input.text.toString())
                when {
                    address == null -> Toast.makeText(this, invalid, Toast.LENGTH_LONG).show()
                    address != current -> {
                        prefs().edit().putString(key, address).apply()
                        restartDaemon()
                        render()
                    }
                }
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    /** The proxy's password: copy it for another app's settings, or make a new one. */
    private fun askProxyPassword() {
        val password = Settings.appProxyPassword(prefs())
        AlertDialog.Builder(this)
            .setTitle(R.string.settings_app_proxy_password)
            .setMessage(getString(R.string.settings_app_proxy_password_detail, password))
            .setPositiveButton(R.string.settings_copy) { _, _ -> copy(password, sensitive = true) }
            .setNeutralButton(R.string.settings_app_proxy_new_password) { _, _ ->
                Settings.newAppProxyPassword(prefs())
                restartDaemon()
                render()
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    /** Onto the clipboard; a password is marked sensitive, so the keyboard's clipboard hides it. */
    private fun copy(text: String, sensitive: Boolean) {
        val clip = ClipData.newPlainText(null, text)
        if (sensitive) clip.description.extras = PersistableBundle().apply { putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true) }
        getSystemService(ClipboardManager::class.java)?.setPrimaryClip(clip)
        // From Android 13 the system says so itself.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) Toast.makeText(this, R.string.browser_copied, Toast.LENGTH_SHORT).show()
    }

    private fun showLicenses() {
        val font = runCatching { assets.open("licenses/space-mono-OFL.txt").bufferedReader().use { it.readText() } }.getOrDefault("")
        val text = TextView(this).apply {
            text = getString(R.string.settings_licenses_text) + font
            setTextIsSelectable(true)
            setTextColor(getColor(R.color.ink_2))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 13f)
            setPadding(dp(20), dp(8), dp(20), dp(8))
        }
        AlertDialog.Builder(this)
            .setTitle(R.string.settings_licenses)
            .setView(ScrollView(this).apply { addView(text) })
            .setPositiveButton(android.R.string.ok, null)
            .show()
    }

    private fun singleChoice(title: Int, items: List<CharSequence>, current: Int, chosen: (Int) -> Unit) =
        pickOne(getString(title), items, current, chosen = chosen)

    private fun textField(value: String, hint: Int) = EditText(this).apply {
        setText(value)
        this.hint = getString(hint)
        inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
        isSingleLine = true
    }

    private fun framed(input: View) = FrameLayout(this).apply {
        setPadding(dp(20), dp(4), dp(20), 0)
        addView(input)
    }

    /** The daemon reads these settings at start. The browser's proxy follows it to its new port. */
    private fun restartDaemon() {
        stopService(Intent(this, OmnidService::class.java))
        startForegroundService(Intent(this, OmnidService::class.java))
        Toast.makeText(this, R.string.settings_restarting, Toast.LENGTH_SHORT).show()
    }

    private fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()

    companion object {
        /** Opens the I2P router's dialog at once: the search page's way to it. */
        const val EXTRA_I2P_ROUTER = "i2p_router"
        private const val SOURCE = "https://github.com/amiriteorg/omnid"
    }
}

/**
 * A dialog that picks one of [items], [current] checked; picking one closes it. Radio buttons in
 * a list of their own rather than the platform's single-choice list, which cuts off items of two
 * lines.
 */
internal fun Activity.pickOne(
    title: CharSequence,
    items: List<CharSequence>,
    current: Int,
    message: CharSequence? = null,
    chosen: (Int) -> Unit,
) {
    fun dp(value: Int) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), resources.displayMetrics).toInt()
    val group = RadioGroup(this).apply { setPadding(dp(12), dp(8), dp(20), dp(8)) }
    var dialog: AlertDialog? = null
    items.forEachIndexed { i, item ->
        group.addView(RadioButton(this).apply {
            id = View.generateViewId()
            text = item
            isChecked = i == current
            setTextColor(getColor(R.color.ink))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            buttonTintList = ColorStateList.valueOf(getColor(R.color.accent))
            setPadding(dp(12), dp(10), 0, dp(10))
            setOnClickListener {
                dialog?.dismiss()
                chosen(i)
            }
        }, RadioGroup.LayoutParams(RadioGroup.LayoutParams.MATCH_PARENT, RadioGroup.LayoutParams.WRAP_CONTENT))
    }
    if (message != null) {
        group.addView(TextView(this).apply {
            text = message
            setTextColor(getColor(R.color.ink_2))
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 14f)
            setPadding(dp(12), 0, 0, dp(8))
        }, 0)
    }
    dialog = AlertDialog.Builder(this)
        .setTitle(title)
        .setView(ScrollView(this).apply { addView(group) })
        .setNegativeButton(android.R.string.cancel, null)
        .show()
}

/** A choice's name, and under it, smaller and quieter, what it means. */
internal fun Context.twoLine(name: String, about: String): CharSequence = SpannableStringBuilder().apply {
    append(name, StyleSpan(Typeface.BOLD), Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
    append("\n")
    val start = length
    append(about)
    setSpan(ForegroundColorSpan(getColor(R.color.muted)), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
    setSpan(RelativeSizeSpan(0.87f), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
}

internal fun Context.levelName(level: SecurityLevel) = getString(
    when (level) {
        SecurityLevel.STANDARD -> R.string.browser_level_standard
        SecurityLevel.SAFER -> R.string.browser_level_safer
        SecurityLevel.SAFEST -> R.string.browser_level_safest
    },
)

/** Each security level's name and what it turns off, for a single-choice list. */
internal fun Context.levelChoices(): List<CharSequence> = SecurityLevel.entries.map { level ->
    val about = when (level) {
        SecurityLevel.STANDARD -> R.string.browser_level_standard_about
        SecurityLevel.SAFER -> R.string.browser_level_safer_about
        SecurityLevel.SAFEST -> R.string.browser_level_safest_about
    }
    twoLine(levelName(level), getString(about))
}
