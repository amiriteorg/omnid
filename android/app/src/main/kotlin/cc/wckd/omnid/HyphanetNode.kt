package cc.wckd.omnid

import java.net.URI

/**
 * Where omnid reaches Hyphanet: a node's FProxy. Empty means a node on this phone (the daemon's
 * default, 127.0.0.1:8888); otherwise one elsewhere, such as the owner's computer over Tailscale.
 * Plain Kotlin, so it is unit-tested on the JVM.
 */
internal object HyphanetNode {
    /** The preference, in [CrawlPolicy.PREFS], that [OmnidService] starts the daemon with. */
    const val KEY = "hyphanet_node"

    /**
     * What was typed as a node's address, as `http://host:port/`, with http and FProxy's port
     * 8888 filled in when left out; "" for none (a node on this phone); null when it is not a
     * node's address.
     */
    fun parse(typed: String): String? {
        val text = typed.trim()
        if (text.isEmpty()) return ""
        val uri = runCatching { URI(if ("://" in text) text else "http://$text") }.getOrNull() ?: return null
        val scheme = uri.scheme?.lowercase()
        if (scheme != "http" && scheme != "https" || uri.host.isNullOrEmpty()) return null
        if (uri.rawUserInfo != null || uri.rawQuery != null || uri.rawFragment != null) return null
        if (!uri.rawPath.isNullOrEmpty() && uri.rawPath != "/") return null
        val port = if (uri.port != -1) uri.port else if (scheme == "https") 443 else 8888
        return "$scheme://${uri.host}:$port/"
    }
}
