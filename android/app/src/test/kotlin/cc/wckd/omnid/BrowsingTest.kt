package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class BrowsingTest {
    @Test
    fun opensTabsNextToTheirPageAndClosesToANeighbour() {
        val tabs = TabList<String>(max = 3)
        assertTrue(tabs.add("a"))
        assertTrue(tabs.add("b"))
        tabs.select("a")
        // A link opened from a in a new tab sits right after it.
        assertTrue(tabs.add("c", background = true))
        assertEquals(listOf("a", "c", "b"), tabs.all)
        assertEquals("a", tabs.shown)
        assertFalse("three is the most", tabs.add("d"))
        // Closing the shown tab shows the one after it; at the end, the one before.
        assertEquals("c", tabs.close("a"))
        tabs.select("b")
        assertEquals("c", tabs.close("b"))
        assertNull(tabs.close("c"))
        assertEquals(-1, tabs.current)
        assertTrue(tabs.add("e"))
        assertEquals("e", tabs.shown)
    }

    @Test
    fun closingAnotherTabKeepsTheShownOne() {
        val tabs = TabList<String>(max = 5)
        listOf("a", "b", "c").forEach { tabs.add(it) }
        assertEquals("c", tabs.close("a"))
        assertEquals(1, tabs.current)
    }

    @Test
    fun saferRunsScriptsOnlyWhereThePageCannotBeChangedOnTheWay() {
        val onion = "http://2gzyxa5ihm7nsggfxnu52rck2vv4rvmdlkiu3zzui5du4xyclen53wid.onion/"
        assertTrue(SecurityLevel.STANDARD.allowsJavascript("http://example.com/"))
        for (url in listOf("https://example.com/", onion, "http://stats.i2p/", "http://hyphanet/USK@a,b,AQACAAE/s/1/", "about:blank")) {
            assertTrue(url, SecurityLevel.SAFER.allowsJavascript(url))
        }
        assertFalse(SecurityLevel.SAFER.allowsJavascript("http://example.com/"))
        assertFalse(SecurityLevel.SAFEST.allowsJavascript("https://example.com/"))
        assertEquals(SecurityLevel.SAFER, SecurityLevel.from("SAFER"))
        assertEquals(SecurityLevel.STANDARD, SecurityLevel.from("nonsense"))
    }

    @Test
    fun everyLevelSetsTheSamePrefs() {
        val names = SecurityLevel.STANDARD.prefs.keys
        for (level in SecurityLevel.entries) assertEquals(level.name, names, level.prefs.keys)
        assertEquals(false, SecurityLevel.SAFER.prefs["javascript.options.ion"])
        assertEquals(true, SecurityLevel.SAFEST.prefs["svg.disabled"])
        assertEquals(1, SecurityLevel.STANDARD.prefs["browser.display.use_document_fonts"])
    }

    @Test
    fun keepsBookmarksAsTheyWereSaved() {
        val page = Bookmark("http://stats.i2p/", "Tab\tand \\ newline\nin a title")
        val other = Bookmark("https://example.com/", "Example")
        val saved = Bookmarks.serialize(listOf(page, other))
        assertEquals(listOf(page, other), Bookmarks.parse(saved))
        // Toggling takes a page out, and puts it back at the top.
        assertEquals(listOf(other), Bookmarks.toggle(listOf(page, other), page))
        assertEquals(listOf(page, other), Bookmarks.toggle(listOf(other), page))
        // Lines that are not web addresses are dropped, and duplicates.
        assertEquals(listOf(other), Bookmarks.parse("javascript:alert(1)\tx\nhttps://example.com/\tExample\nhttps://example.com/\tAgain"))
        assertEquals(emptyList<Bookmark>(), Bookmarks.parse(null))
    }
}
