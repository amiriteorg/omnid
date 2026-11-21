package cc.wckd.omnid

import java.net.URI

/**
 * How much of the web platform pages get, as in Tor Browser: Standard everything (still with
 * resist-fingerprinting), Safer no JavaScript on pages that came over plain HTTP from the
 * clearnet and no JIT, WebAssembly, MathML or SVG fonts anywhere, Safest no JavaScript at all and
 * neither SVG nor the page's own fonts. Plain Kotlin, unit-tested.
 */
internal enum class SecurityLevel {
    STANDARD,
    SAFER,
    SAFEST;

    /**
     * Whether a page at [url] may run JavaScript. Safer allows it where the page cannot be
     * changed on the way: HTTPS, onion services, I2P and Hyphanet.
     */
    fun allowsJavascript(url: String?): Boolean = when (this) {
        STANDARD -> true
        SAFEST -> false
        SAFER -> {
            val uri = runCatching { URI(url) }.getOrNull()
            val host = uri?.host?.lowercase()?.trimEnd('.').orEmpty()
            uri?.scheme.equals("https", ignoreCase = true) || host.endsWith(".onion") ||
                BrowserConfig.isI2p(url) || BrowserConfig.isHyphanet(url) ||
                // Gecko's own pages, and the pages the browser makes itself (errors).
                uri?.scheme in setOf("about", "data")
        }
    }

    /** Gecko prefs for this level, every one of them, so moving down a level restores them. */
    val prefs: Map<String, Any>
        get() {
            val safer = this != STANDARD
            val safest = this == SAFEST
            return linkedMapOf(
                "javascript.options.ion" to !safer,
                "javascript.options.baselinejit" to !safer,
                "javascript.options.wasm" to !safer,
                "mathml.disabled" to safer,
                "gfx.font_rendering.opentype_svg.enabled" to !safer,
                // 1: audible media waits for a tap; 5: all media does.
                "media.autoplay.default" to if (safer) 5 else 1,
                "svg.disabled" to safest,
                "browser.display.use_document_fonts" to if (safest) 0 else 1,
            )
        }

    companion object {
        const val KEY = "browser_security_level"

        fun from(name: String?): SecurityLevel = entries.firstOrNull { it.name == name } ?: STANDARD
    }
}
