package cc.wckd.omnid

/**
 * The third-party notices a release build carries in [ASSET], written by
 * `scripts/third-party-notices.py`, cut into paragraphs for a list: one text view cannot lay out
 * the file's 400 KB. A paragraph's lines are joined, since a phone's line is narrower than the
 * file's. A block with a rule of `=` or `-` in it is a heading.
 */
object Notices {
    const val ASSET = "licenses/third-party.txt"

    data class Paragraph(val text: String, val heading: Boolean)

    fun paragraphs(text: String): List<Paragraph> =
        text.split(BLANK).mapNotNull { block ->
            val lines = block.lines().map(String::trim).filter(String::isNotEmpty)
            val words = lines.filterNot(RULE::matches)
            if (words.isEmpty()) null else Paragraph(words.joinToString(" "), heading = words.size < lines.size)
        }

    private val BLANK = Regex("\n[ \t]*\n")
    private val RULE = Regex("[=-]{3,}")
}
