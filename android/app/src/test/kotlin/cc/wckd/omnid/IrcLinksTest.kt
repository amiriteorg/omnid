package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class IrcLinksTest {
    @Test
    fun handsOnServersAndChannels() {
        for (link in listOf(
            "irc://irc.libera.chat/",
            "ircs://irc.libera.chat/#rust",
            "ircs://irc.libera.chat:7000/#rust",
            "irc://irc.postman.i2p/#i2p-chat",
            "ircs://oftcnet6xg6roj6d7id4y4cu6dchysacqj2ldgea73qzdagufflqxrid.onion/#tor",
            "irc://irc.example/#caf%C3%A9",
            "irc://irc.example/&local",
        )) {
            assertEquals(link, IrcLinks.safe(" $link "))
        }
    }

    @Test
    fun neverAPasswordAKeyOrAnythingElse() {
        for (link in listOf(
            "irc://me:secret@irc.example/#a", // a password
            "irc://irc.example/#a?key", // a channel key
            "irc://irc.example/#a,needkey",
            "irc://irc.example:99999/#a",
            "irc://irc.example/#a b",
            "https://web.libera.chat/#rust",
            "irc://irc.example", // not as the daemon writes it
        )) {
            assertNull(link, IrcLinks.safe(link))
        }
    }
}
