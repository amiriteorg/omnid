package cc.wckd.omnid

import java.net.URI

/**
 * Where omnid reaches Hyphanet: a node's FProxy. Empty means a node on this phone (the daemon's
 * default, 127.0.0.1:8888); otherwise one elsewhere, such as the owner's computer over Tailscale,
 * with one on this phone as the fallback. Plain Kotlin, so it is unit-tested on the JVM.
 */
internal object HyphanetNode {
    /** The preference, in [CrawlPolicy.PREFS], that [OmnidService] starts the daemon with. */
    const val KEY = "hyphanet_node"

    /** A node's address as `http://host:port/`; see [routerAddress]. FProxy's port is 8888. */
    fun parse(typed: String): String? = routerAddress(typed, 8888, setOf("http", "https"))
}

/**
 * Where omnid reaches I2P: a router's HTTP proxy. Empty means the i2pd app on this phone (the
 * daemon's default, 127.0.0.1:4444); otherwise a router elsewhere, such as the owner's computer
 * over Tailscale, asked first, with the i2pd app as the fallback.
 */
internal object I2pRouter {
    /** The preference, in [CrawlPolicy.PREFS], that [OmnidService] starts the daemon with. */
    const val KEY = "i2p_router"

    /** A router's address as `http://host:port/`; see [routerAddress]. i2pd's proxy is plain HTTP on 4444. */
    fun parse(typed: String): String? = routerAddress(typed, 4444, setOf("http"))
}

/**
 * Where omnid reaches IPFS: an IPFS node's gateway elsewhere, such as kubo's on the owner's
 * computer over Tailscale. Empty means none: no IPFS node runs on a phone.
 */
internal object IpfsGateway {
    /** The preference, in [CrawlPolicy.PREFS], that [OmnidService] starts the daemon with. */
    const val KEY = "ipfs_gateway"

    /** A gateway's address as `http://host:port/`; see [routerAddress]. kubo's gateway is on 8080. */
    fun parse(typed: String): String? = routerAddress(typed, 8080, setOf("http", "https"))
}

/**
 * What was typed as a router's address, as `scheme://host:port/`, with http and [defaultPort]
 * filled in when left out; "" for none (the one on this phone); null when it is not a router's
 * address.
 */
private fun routerAddress(typed: String, defaultPort: Int, schemes: Set<String>): String? {
    val text = typed.trim()
    if (text.isEmpty()) return ""
    val uri = runCatching { URI(if ("://" in text) text else "http://$text") }.getOrNull() ?: return null
    val scheme = uri.scheme?.lowercase()
    if (scheme !in schemes || uri.host.isNullOrEmpty()) return null
    if (uri.rawUserInfo != null || uri.rawQuery != null || uri.rawFragment != null) return null
    if (!uri.rawPath.isNullOrEmpty() && uri.rawPath != "/") return null
    val port = if (uri.port != -1) uri.port else if (scheme == "https") 443 else defaultPort
    return "$scheme://${uri.host}:$port/"
}
