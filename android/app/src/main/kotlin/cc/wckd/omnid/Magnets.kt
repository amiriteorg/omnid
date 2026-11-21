package cc.wckd.omnid

import java.net.URLDecoder
import java.net.URLEncoder

/**
 * Magnet links handed to a torrent app, which reaches the BitTorrent network directly, outside
 * Tor. Plain Kotlin, so the rules are unit-tested on the JVM.
 */
internal object Magnets {
    private val HASH = Regex("""^urn:(btih:([0-9a-fA-F]{40}|[A-Za-z2-7]{32})|btmh:1220[0-9a-fA-F]{64})$""")

    /**
     * [link] with nothing but its info hash and display name: a page can put its own trackers
     * (`tr`), peers (`x.pe`) and sources (`ws`, `as`, `xs`) into a link, so a torrent app that
     * announces to them tells the page's author who opened it. Null when it has no valid hash.
     */
    fun safe(link: String): String? {
        if (!link.startsWith("magnet:?", ignoreCase = true)) return null
        val params = link.substringAfter('?').split('&').mapNotNull { pair ->
            val (key, value) = pair.split('=', limit = 2).takeIf { it.size == 2 } ?: return@mapNotNull null
            val decoded = runCatching { URLDecoder.decode(value.replace("+", "%2B"), "UTF-8") }.getOrNull() ?: return@mapNotNull null
            key.lowercase() to decoded
        }
        val hash = params.firstOrNull { (key, value) -> key == "xt" && HASH.matches(value) }?.second ?: return null
        val name = params.firstOrNull { it.first == "dn" }?.second?.take(200)
        return buildString {
            append("magnet:?xt=").append(hash)
            if (!name.isNullOrBlank()) append("&dn=").append(URLEncoder.encode(name, "UTF-8").replace("+", "%20"))
        }
    }
}
