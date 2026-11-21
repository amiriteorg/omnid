package cc.wckd.omnid

import java.net.URI
import java.net.URLDecoder

/**
 * What the browser decides about a download before it starts: the file's name, whether the
 * daemon fetches it in parallel ranges or it comes over the browser's own connection, and how many
 * connections Settings allows. Plain Kotlin, so the rules are unit-tested on the JVM.
 */
internal object Downloads {
    /** Settings → Download connections: how many at once, each on a Tor circuit of its own. */
    const val CONNECTIONS = "download_connections"
    val CONNECTION_CHOICES = listOf(1, 2, 4, 6, 8)
    const val DEFAULT_CONNECTIONS = 4

    fun connections(stored: Int): Int = stored.takeIf { it in CONNECTION_CHOICES } ?: DEFAULT_CONNECTIONS

    /** Below this a file comes over the browser's own connection: ranges would cost more circuits than they save. */
    const val MIN_PARALLEL = 2L shl 20

    /** Whether the daemon fetches it in ranges: the server serves them, and the size is known and large enough. */
    fun parallel(length: Long?, acceptRanges: String?, connections: Int): Boolean =
        connections > 1 && length != null && length >= MIN_PARALLEL &&
            acceptRanges?.trim().equals("bytes", ignoreCase = true)

    /** A header from GeckoView's map, whatever case the server wrote it in. */
    fun header(headers: Map<String, String>, name: String): String? =
        headers.entries.firstOrNull { it.key.equals(name, ignoreCase = true) }?.value

    /** Content-Length as a size, null when it is missing or not a number. */
    fun length(headers: Map<String, String>): Long? = header(headers, "Content-Length")?.trim()?.toLongOrNull()?.takeIf { it >= 0 }

    /**
     * The file's name: Content-Disposition's `filename*` (RFC 6266, percent-encoded UTF-8) or
     * `filename`, else the address's last path segment, else "download", made safe ([safe]) and
     * given an extension from [mimeType] when it has none.
     */
    fun fileName(disposition: String?, url: String, mimeType: String?): String {
        val fromHeader = disposition?.let(::dispositionName)
        val fromUrl = runCatching { URI(url).rawPath }.getOrNull()
            ?.substringAfterLast('/')
            ?.let { runCatching { URLDecoder.decode(it.replace("+", "%2B"), "UTF-8") }.getOrNull() }
        val name = safe(fromHeader?.takeIf { it.isNotBlank() } ?: fromUrl.orEmpty())
        val extension = EXTENSIONS[mimeType?.substringBefore(';')?.trim()?.lowercase()]
        return if (extension != null && !name.contains('.')) "$name.$extension" else name
    }

    private fun dispositionName(disposition: String): String? {
        Regex("""(?i)(?:^|;)\s*filename\*\s*=\s*([\w!#$%&+.^`|~-]*)'[^']*'([^;\s]+)""").find(disposition)?.let { match ->
            val charset = match.groupValues[1].ifEmpty { "UTF-8" }
            runCatching { return URLDecoder.decode(match.groupValues[2].replace("+", "%2B"), charset) }
        }
        val plain = Regex("""(?i)(?:^|;)\s*filename\s*=\s*(?:"((?:[^"\\]|\\.)*)"|([^;]+))""").find(disposition) ?: return null
        return plain.groupValues[1].takeIf { it.isNotEmpty() }?.replace(Regex("""\\(.)"""), "$1") ?: plain.groupValues[2].trim()
    }

    private const val MAX_NAME = 120

    /**
     * [name] as a file's name: no folders, control characters or characters file systems refuse,
     * no leading dots (a hidden file) or trailing dots and spaces, at most [MAX_NAME] characters
     * with the extension kept; "download" when nothing is left.
     */
    fun safe(name: String): String {
        val base = name.substringAfterLast('/').substringAfterLast('\\')
        val clean = buildString {
            for (c in base) append(if (c.code < 0x20 || c.code == 0x7f || c in "<>:\"|?*") '_' else c)
        }.trim().trimStart('.').trimEnd('.', ' ')
        if (clean.isEmpty()) return "download"
        if (clean.length <= MAX_NAME) return clean
        val extension = clean.substringAfterLast('.', "").takeIf { it.length in 1..10 }?.let { ".$it" }.orEmpty()
        return clean.take(MAX_NAME - extension.length) + extension
    }

    /** Extensions for names that have none, by type: the common ones only. */
    private val EXTENSIONS = mapOf(
        "application/pdf" to "pdf",
        "application/zip" to "zip",
        "application/epub+zip" to "epub",
        "application/x-bittorrent" to "torrent",
        "application/gzip" to "gz",
        "application/x-7z-compressed" to "7z",
        "image/jpeg" to "jpg",
        "image/png" to "png",
        "image/gif" to "gif",
        "image/webp" to "webp",
        "video/mp4" to "mp4",
        "video/webm" to "webm",
        "audio/mpeg" to "mp3",
        "audio/ogg" to "ogg",
        "text/plain" to "txt",
    )
}
