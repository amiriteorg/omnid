package cc.wckd.omnid

/**
 * The app's settings: their keys in [CrawlPolicy.PREFS], their choices, and how stored or typed
 * values read back. Plain Kotlin, so the rules are unit-tested on the JVM; [SettingsActivity] is
 * the screen.
 */
internal object Settings {
    /** Light or dark for OMNId's own screens and the search page; System follows the phone. */
    enum class Theme { SYSTEM, LIGHT, DARK }

    const val THEME = "theme"

    fun theme(stored: String?): Theme = Theme.entries.firstOrNull { it.name == stored } ?: Theme.SYSTEM

    /** Text size on web pages, as a factor of the page's own; the phone's font scale still applies. */
    val TEXT_SIZES = listOf(0.85f, 1f, 1.15f, 1.3f, 1.5f)
    const val TEXT_SIZE = "text_size"

    fun textSize(stored: Float): Float = TEXT_SIZES.firstOrNull { it == stored } ?: 1f

    /** Whether web pages load images; off saves most of a page's bytes over Tor. */
    const val IMAGES = "load_images"

    /** Whether screenshots (and the recent-apps thumbnail) may show the browser. Off by default. */
    const val SCREENSHOTS = "browser_screenshots"

    /** How hard the phone crawls when it may: the daemon scales its connections per network. */
    enum class CrawlSpeed { GENTLE, NORMAL, FAST }

    const val CRAWL_SPEED = "crawl_speed"

    fun crawlSpeed(stored: String?): CrawlSpeed = CrawlSpeed.entries.firstOrNull { it.name == stored } ?: CrawlSpeed.NORMAL

    /** The search page's networks, in its own words (app.js), in the order its chips show. */
    val NETWORKS = listOf("onion", "clearnet", "magnet", "i2p", "hyphanet")
    const val SEARCH_NETWORKS = "search_networks"

    /** The networks a search starts with: every one unless some were stored; never none. */
    fun networks(stored: String?): Set<String> {
        val chosen = stored.orEmpty().split(',').map(String::trim).filter { it in NETWORKS }.toSet()
        return chosen.ifEmpty { NETWORKS.toSet() }
    }

    fun storeNetworks(chosen: Set<String>): String = NETWORKS.filter { it in chosen }.joinToString(",")

    /** Languages to rank first, as typed: "" for the phone's own. */
    const val SEARCH_LANGUAGES = "search_languages"
    private const val MAX_LANGUAGES = 5

    /**
     * What was typed as languages to rank first, as the daemon's `lang` parameter: primary
     * language subtags ("de, EN-gb" → "de,en"), at most five, in order, without repeats; "" for
     * none (the phone's own languages); null when something is not a language.
     */
    fun languages(typed: String): String? {
        val tags = typed.split(',', ' ', ';').map(String::trim).filter(String::isNotEmpty)
        val primary = tags.map { it.substringBefore('-').substringBefore('_').lowercase() }
        if (primary.any { it.length !in 2..3 || !it.all { c -> c in 'a'..'z' } }) return null
        val distinct = primary.distinct()
        if (distinct.size > MAX_LANGUAGES) return null
        return distinct.joinToString(",")
    }

    /** Files in the daemon's data directory it reads lists from at start (`omnid-android`). */
    const val BLOCKED_WORDS_FILE = "blocked-terms.txt"
    const val BLOCKED_SITES_FILE = "denylist.txt"
    private const val MAX_ENTRIES = 1_000
    private const val MAX_ENTRY = 200

    /**
     * A list as typed into its editor, one entry per line: trimmed, blank lines and repeats
     * dropped, `#` lines (the file's comments) dropped, at most [MAX_ENTRIES] of at most
     * [MAX_ENTRY] characters.
     */
    fun entries(text: String): List<String> = text.lines()
        .map(String::trim)
        .filter { it.isNotEmpty() && !it.startsWith('#') && it.length <= MAX_ENTRY }
        .distinct()
        .take(MAX_ENTRIES)

    /** The search page for these settings, with [query] already searched if there is one. */
    fun searchPage(origin: String, networks: Set<String>, languages: String, query: String? = null): String = buildString {
        append(origin).append("/?embedded")
        if (networks != NETWORKS.toSet()) append("&networks=").append(storeNetworks(networks))
        if (languages.isNotEmpty()) append("&lang=").append(languages)
        if (!query.isNullOrBlank()) append("&q=").append(java.net.URLEncoder.encode(query.trim(), "UTF-8").replace("+", "%20"))
    }

    /** The Gecko prefs the settings ask for: the security level's, and whether images load. */
    fun geckoPrefs(level: SecurityLevel, images: Boolean): Map<String, Any> =
        level.prefs + ("permissions.default.image" to if (images) 1 else 2)
}
