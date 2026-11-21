package cc.wckd.omnid

import android.content.Context
import java.io.File
import org.json.JSONObject
import org.mozilla.geckoview.ContentBlocking
import org.mozilla.geckoview.GeckoPreferenceController
import org.mozilla.geckoview.GeckoResult
import org.mozilla.geckoview.GeckoRuntime
import org.mozilla.geckoview.GeckoRuntimeSettings
import org.mozilla.geckoview.WebExtension

/**
 * The process's one GeckoRuntime. It is only handed out once Gecko has confirmed the prefs that
 * keep traffic inside omnid's proxy and the proxy extension is installed; if either fails, every
 * caller gets the failure and no page ever loads. Call on the UI thread.
 */
internal object Browser {
    private const val EXTENSION_ID = "browser-proxy@omnid.wckd.cc"
    private const val EXTENSION_URI = "resource://android/assets/browser-proxy/"
    /** The name the extension's `runtime.sendNativeMessage` uses. */
    private const val NATIVE_APP = "omnid"

    private var ready: GeckoResult<GeckoRuntime>? = null

    fun ready(context: Context): GeckoResult<GeckoRuntime> = ready ?: start(context).also { ready = it }

    private fun start(context: Context): GeckoResult<GeckoRuntime> {
        // GeckoView reads prefs from this file before Gecko starts, so no request goes out before
        // they apply. It ignores a file it cannot read, hence the check that follows.
        val config = File(context.filesDir, "browser/geckoview-config.yaml")
        config.parentFile?.mkdirs()
        config.writeText(BrowserConfig.yaml(BrowserConfig.prefs))
        val blocking = ContentBlocking.Settings.Builder()
            .antiTracking(ContentBlocking.AntiTracking.STRICT)
            .enhancedTrackingProtectionLevel(ContentBlocking.EtpLevel.STRICT)
            .cookieBehavior(ContentBlocking.CookieBehavior.ACCEPT_FIRST_PARTY_AND_ISOLATE_OTHERS)
            .cookieBehaviorPrivateMode(ContentBlocking.CookieBehavior.ACCEPT_FIRST_PARTY_AND_ISOLATE_OTHERS)
            // Safe Browsing would send lookups to Google.
            .safeBrowsing(ContentBlocking.SafeBrowsing.NONE)
            .build()
        val settings = GeckoRuntimeSettings.Builder()
            .configFilePath(config.path)
            .contentBlocking(blocking)
            // HTTPS only on the clearnet, where an exit could read or change plain HTTP; onion
            // addresses are exempt, their connection is end-to-end encrypted already.
            .allowInsecureConnections(GeckoRuntimeSettings.HTTPS_ONLY)
            .globalPrivacyControlEnabled(true)
            .loginAutofillEnabled(false)
            .remoteDebuggingEnabled(false)
            .aboutConfigEnabled(false)
            .consoleOutput(false)
            // The text size setting is a factor Gecko applies itself, not the phone's font scale.
            .automaticFontSizeAdjustment(false)
            .build()
        val runtime = try {
            GeckoRuntime.create(context.applicationContext, settings)
        } catch (e: Throwable) {
            return GeckoResult.fromException(e)
        }
        return verifyPrefs().then { installProxy(runtime) }.then { applySettings(context, runtime) }.map { runtime }
    }

    /**
     * Takes the settings into the running Gecko: the security level's and the image setting's
     * prefs ([apply]), the text size and the color scheme pages are offered.
     */
    fun applySettings(context: Context, runtime: GeckoRuntime): GeckoResult<Void> {
        val prefs = context.getSharedPreferences(CrawlPolicy.PREFS, Context.MODE_PRIVATE)
        runtime.settings.setFontSizeFactor(Settings.textSize(prefs.getFloat(Settings.TEXT_SIZE, 1f)))
        runtime.settings.setPreferredColorScheme(
            when (Settings.theme(prefs.getString(Settings.THEME, null))) {
                Settings.Theme.SYSTEM -> GeckoRuntimeSettings.COLOR_SCHEME_SYSTEM
                Settings.Theme.LIGHT -> GeckoRuntimeSettings.COLOR_SCHEME_LIGHT
                Settings.Theme.DARK -> GeckoRuntimeSettings.COLOR_SCHEME_DARK
            },
        )
        val level = SecurityLevel.from(prefs.getString(SecurityLevel.KEY, null))
        return apply(Settings.geckoPrefs(level, prefs.getBoolean(Settings.IMAGES, true)))
    }

    /**
     * Sets [wanted] in the running Gecko and reads them back; pages loaded afterwards get them.
     * Whether pages run JavaScript at all is each tab's session setting, not a pref.
     */
    fun apply(wanted: Map<String, Any>): GeckoResult<Void> {
        val set = wanted.map { (name, value) ->
            when (value) {
                is Boolean -> GeckoPreferenceController.setGeckoPref(name, value, GeckoPreferenceController.PREF_BRANCH_USER)
                is Int -> GeckoPreferenceController.setGeckoPref(name, value, GeckoPreferenceController.PREF_BRANCH_USER)
                else -> GeckoResult.fromException(IllegalArgumentException("$name: ${value::class.simpleName} is not a pref type"))
            }
        }
        return GeckoResult.allOf(set).then { GeckoPreferenceController.getGeckoPrefs(wanted.keys.toList()) }.map<Void> { reported ->
            val actual = reported.orEmpty().associate { it.pref to it.value }
            val wrong = wanted.keys.filter { actual[it] != wanted[it] }
            check(wrong.isEmpty()) { "Gecko did not take ${wrong.joinToString()}" }
            null
        }
    }

    private fun verifyPrefs(): GeckoResult<Void> =
        GeckoPreferenceController.getGeckoPrefs(BrowserConfig.critical).map<Void> { reported ->
            val actual = reported.orEmpty().associate { it.pref to it.value }
            val wrong = BrowserConfig.mismatched(actual)
            check(wrong.isEmpty()) { "Gecko did not take ${wrong.joinToString()}" }
            null
        }

    private fun installProxy(runtime: GeckoRuntime): GeckoResult<Void> {
        val extensions = runtime.webExtensionController
        return extensions.ensureBuiltIn(EXTENSION_URI, EXTENSION_ID).then { extension ->
            val proxy = checkNotNull(extension) { "the proxy extension did not install" }
            proxy.setMessageDelegate(ProxyConfig, NATIVE_APP)
            // Every browser tab is private; the extension must route those too.
            extensions.setAllowedInPrivateBrowsing(proxy, true)
        }.map<Void> { null }
    }

    /** Answers the extension's request for the proxy's port and password. */
    private object ProxyConfig : WebExtension.MessageDelegate {
        override fun onMessage(nativeApp: String, message: Any, sender: WebExtension.MessageSender): GeckoResult<Any>? {
            if (message != "proxy" || sender.environmentType != WebExtension.MessageSender.ENV_TYPE_EXTENSION) {
                return null
            }
            val endpoint = OmnidService.endpoint
            if (endpoint == null || endpoint.browserProxyPort <= 0) {
                return GeckoResult.fromException<Any>(IllegalStateException("omnid is not running"))
            }
            // As JSON text: the bridge back to the extension carries plain values, not JSONObject.
            return GeckoResult.fromValue<Any>(
                JSONObject().put("port", endpoint.browserProxyPort).put("password", endpoint.token).toString(),
            )
        }
    }
}
