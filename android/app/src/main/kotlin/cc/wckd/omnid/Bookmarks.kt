package cc.wckd.omnid

/**
 * Pages kept on purpose: the one thing the private browser remembers, and only when asked. Kept
 * in the app's own storage as lines of address and title. Plain Kotlin, unit-tested.
 */
internal data class Bookmark(val url: String, val title: String)

internal object Bookmarks {
    const val KEY = "browser_bookmarks"
    /** Enough to be useful, few enough that a damaged store stays small. */
    const val MAX = 500

    fun parse(text: String?): List<Bookmark> =
        text.orEmpty().lineSequence()
            .mapNotNull { line ->
                val url = line.substringBefore('\t')
                val title = line.substringAfter('\t', "")
                Bookmark(url, unescape(title)).takeIf { BrowserConfig.address(url) == url }
            }
            .distinctBy { it.url }
            .take(MAX)
            .toList()

    fun serialize(bookmarks: List<Bookmark>): String =
        bookmarks.take(MAX).joinToString("\n") { "${it.url}\t${escape(it.title)}" }

    /** [bookmarks] with [page] added at the top, or taken out if it was there. */
    fun toggle(bookmarks: List<Bookmark>, page: Bookmark): List<Bookmark> =
        if (bookmarks.any { it.url == page.url }) bookmarks.filterNot { it.url == page.url }
        else (listOf(page) + bookmarks).take(MAX)

    private fun escape(text: String) = text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")

    private fun unescape(text: String): String = buildString {
        var i = 0
        while (i < text.length) {
            val c = text[i]
            if (c == '\\' && i + 1 < text.length) {
                append(when (text[i + 1]) { 't' -> '\t'; 'n' -> '\n'; else -> text[i + 1] })
                i += 2
            } else {
                append(c)
                i++
            }
        }
    }
}
