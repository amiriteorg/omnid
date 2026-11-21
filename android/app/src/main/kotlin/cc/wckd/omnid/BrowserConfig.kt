package cc.wckd.omnid

/**
 * How omnid's browser is set up before GeckoView starts, and what it opens. Plain Kotlin, so the
 * rules are unit-tested on the JVM.
 */
internal object BrowserConfig {
    /**
     * Gecko's own proxy setting, for requests the proxy extension does not route (Gecko's internal
     * requests, or any request while the extension is not running): a port below 1024, which no app
     * can listen on, so they fail instead of leaving unproxied.
     */
    const val DEAD_END_PORT = 9

    /** Gecko prefs, read at startup from the config file GeckoView is pointed at. */
    val prefs: Map<String, Any> = linkedMapOf(
        // Every connection through a SOCKS5 proxy that resolves names itself: nothing direct, no
        // DNS from the phone, localhost included (so pages cannot reach omnid's API).
        "network.proxy.type" to 1,
        "network.proxy.socks" to "127.0.0.1",
        "network.proxy.socks_port" to DEAD_END_PORT,
        "network.proxy.socks_version" to 5,
        "network.proxy.socks_remote_dns" to true,
        "network.proxy.no_proxies_on" to "",
        "network.proxy.allow_hijacking_localhost" to true,
        "network.proxy.failover_direct" to false,
        "network.proxy.allow_bypass" to false,
        "network.dns.disabled" to true,
        "network.trr.mode" to 5,
        "network.http.http3.enable" to false,
        // The phone's own addresses.
        "media.peerconnection.enabled" to false,
        "media.navigator.enabled" to false,
        // Connections nobody asked for.
        "network.prefetch-next" to false,
        "network.dns.disablePrefetch" to true,
        "network.predictor.enabled" to false,
        "network.http.speculative-parallel-limit" to 0,
        "browser.send_pings" to false,
        "network.captive-portal-service.enabled" to false,
        "network.connectivity-service.enabled" to false,
        // Fingerprinting.
        "privacy.resistFingerprinting" to true,
        "webgl.disabled" to true,
        "geo.enabled" to false,
        "dom.battery.enabled" to false,
        // Other sites see where a request came from, not the page.
        "network.http.referer.XOriginTrimmingPolicy" to 2,
        // WebAuthn needs Google Play services, which the app leaves out.
        "security.webauth.webauthn" to false,
    )

    /** Prefs that must have taken effect before a page loads; without them requests could leak. */
    val critical = listOf(
        "network.proxy.type",
        "network.proxy.socks",
        "network.proxy.socks_port",
        "network.proxy.socks_remote_dns",
        "network.proxy.failover_direct",
        "network.proxy.allow_hijacking_localhost",
        "network.dns.disabled",
        "media.peerconnection.enabled",
        "privacy.resistFingerprinting",
    )

    /** The prefs that are not what [prefs] says, given what Gecko reports. */
    fun mismatched(actual: Map<String, Any?>): List<String> = critical.filter { actual[it] != prefs[it] }

    /** [prefs] as GeckoView's YAML config file. */
    fun yaml(prefs: Map<String, Any>): String = buildString {
        appendLine("prefs:")
        for ((name, value) in prefs) {
            val literal = when (value) {
                is String -> quote(value)
                is Boolean, is Int -> value.toString()
                else -> throw IllegalArgumentException("$name: ${value::class.simpleName} is not a pref type")
            }
            appendLine("  ${quote(name)}: $literal")
        }
    }

    private fun quote(text: String) = "\"" + text.replace("\\", "\\\\").replace("\"", "\\\"") + "\""

    /** Web pages, which open in the browser. */
    fun isWebPage(scheme: String?, host: String?): Boolean =
        scheme?.lowercase() in setOf("http", "https") && !host.isNullOrEmpty()

    /** What the browser does when a page asks to go to an address. */
    enum class Target { LOAD, ELSEWHERE, REFUSE }

    fun target(scheme: String?, host: String?): Target = when {
        isWebPage(scheme, host) -> Target.LOAD
        // Gecko's own pages, and what pages make for themselves.
        scheme?.lowercase() in setOf("about", "data", "blob") -> Target.LOAD
        scheme?.lowercase() == "magnet" -> Target.ELSEWHERE
        // tel:, intent:, file:, content: and the rest would leave the browser or read the phone.
        else -> Target.REFUSE
    }

    /**
     * The address bar's text while it is not being edited: host and path, no scheme (the badge
     * beside it says how the page arrived), and an onion's 56 characters cut to both ends, which
     * is what people compare.
     */
    fun displayAddress(url: String): String {
        val bare = url.substringAfter("://", url).trimEnd('/')
        return Regex("""^((?:[a-z0-9-]+\.)*)([a-z2-7]{8})[a-z2-7]{43}([a-z2-7]{5})\.onion""", RegexOption.IGNORE_CASE)
            .replace(bare) { m -> "${m.groupValues[1]}${m.groupValues[2]}…${m.groupValues[3]}.onion" }
    }

    /**
     * What was typed into the address bar as an address: URLs as they are, a bare host with http
     * for `.onion` and `.i2p` (which have no certificates) and https otherwise. `null` for text that
     * is not an address.
     */
    fun address(typed: String): String? {
        val text = typed.trim()
        if (text.isEmpty() || text.any { it.isWhitespace() }) return null
        val scheme = text.substringBefore("://", "").lowercase()
        if (scheme.isNotEmpty()) return if (scheme == "http" || scheme == "https") text else null
        val host = text.substringBefore('/').substringBefore(':').lowercase()
        if (!host.contains('.') || host.startsWith('.') || host.endsWith('.')) return null
        val overlay = host.endsWith(".onion") || host.endsWith(".i2p")
        return (if (overlay) "http://" else "https://") + text
    }
}
