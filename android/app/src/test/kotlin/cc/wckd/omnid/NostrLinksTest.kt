package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class NostrLinksTest {
    private val npub = "nostr:npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg"

    @Test
    fun handsOnNotesProfilesAndArticles() {
        assertEquals(npub, NostrLinks.safe(npub))
        assertEquals(npub, NostrLinks.safe(" NOSTR:NPUB10ELFCS4FR0L0R8AF98JLMGDH9C8TCXJVZ9QKW038JS35MP4DMA8QZVJPTG "))
        assertEquals("nostr:note1qqqqqqqqqqqqqq", NostrLinks.safe("nostr:note1qqqqqqqqqqqqqq"))
    }

    @Test
    fun neverAKeyOrAnythingElse() {
        for (link in listOf(
            "nostr:nsec1qqqqqqqqqqqqqqqq", // a private key
            "nostr:npub1b0", // b, 1 and o are not bech32
            "nostr:npub1qqqq", // too short
            "npub10elfcs4fr0l0r8af98jlmgdh9c8tcxjvz9qkw038js35mp4dma8qzvjptg", // no scheme
            "nostr:npub1qqqqqqqq?relay=wss://watcher.example",
            "https://njump.me/npub1qqqqqqqq",
        )) {
            assertNull(link, NostrLinks.safe(link))
        }
    }
}
