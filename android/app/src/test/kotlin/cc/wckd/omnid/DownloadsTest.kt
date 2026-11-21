package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DownloadsTest {
    @Test
    fun namesTheFileFromTheServerOrTheAddress() {
        val url = "https://example.com/files/report%20final.pdf?x=1"
        assertEquals("report final.pdf", Downloads.fileName(null, url, "application/pdf"))
        assertEquals("plain.txt", Downloads.fileName("attachment; filename=plain.txt", url, null))
        assertEquals("a \"b\".txt".replace('"', '_'), Downloads.fileName("attachment; filename=\"a \\\"b\\\".txt\"", url, null))
        // filename* wins over filename, and is percent-decoded UTF-8.
        assertEquals(
            "naïve file.txt",
            Downloads.fileName("attachment; filename=\"fallback.txt\"; filename*=UTF-8''na%C3%AFve%20file.txt", url, null),
        )
        assertEquals("download.zip", Downloads.fileName(null, "https://example.com/", "application/zip"))
        assertEquals("archive.zip", Downloads.fileName(null, "http://site.onion/get/archive", "application/zip"))
        assertEquals("archive", Downloads.fileName(null, "http://site.onion/get/archive", "application/octet-stream"))
    }

    @Test
    fun makesNamesSafeForAFile() {
        assertEquals("passwd", Downloads.safe("../../etc/passwd"))
        assertEquals("evil.exe", Downloads.safe("C:\\Windows\\evil.exe"))
        assertEquals("bashrc", Downloads.safe(".bashrc"))
        assertEquals("a_b_c.txt", Downloads.safe("a<b>c.txt"))
        assertEquals("tab_name", Downloads.safe("tab\tname"))
        assertEquals("trailing", Downloads.safe("trailing. . "))
        assertEquals("download", Downloads.safe("..."))
        assertEquals("download", Downloads.safe(""))
        val long = Downloads.safe("x".repeat(300) + ".tar.gz")
        assertEquals(120, long.length)
        assertTrue(long, long.endsWith(".gz"))
    }

    @Test
    fun fetchesInRangesOnlyWhenItPays() {
        val big = Downloads.MIN_PARALLEL
        assertTrue(Downloads.parallel(big, "bytes", 4))
        assertTrue(Downloads.parallel(big, " Bytes ", 2))
        assertFalse("too small", Downloads.parallel(big - 1, "bytes", 4))
        assertFalse("no ranges", Downloads.parallel(big, null, 4))
        assertFalse("no ranges", Downloads.parallel(big, "none", 4))
        assertFalse("unknown size", Downloads.parallel(null, "bytes", 4))
        assertFalse("one connection", Downloads.parallel(big, "bytes", 1))
    }

    @Test
    fun readsHeadersInAnyCase() {
        val headers = mapOf("content-length" to " 1234 ", "ACCEPT-RANGES" to "bytes")
        assertEquals(1234L, Downloads.length(headers))
        assertEquals("bytes", Downloads.header(headers, "Accept-Ranges"))
        assertEquals(null, Downloads.length(mapOf("Content-Length" to "-1")))
        assertEquals(4, Downloads.connections(0))
        assertEquals(8, Downloads.connections(8))
    }
}
