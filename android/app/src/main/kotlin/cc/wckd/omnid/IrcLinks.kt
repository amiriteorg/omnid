package cc.wckd.omnid

/**
 * IRC links handed to an IRC app: the search page opens an IRC result as an `irc://` or `ircs://`
 * address, which such an app joins. Plain Kotlin, so the rule is unit-tested on the JVM.
 */
internal object IrcLinks {
    /**
     * A server, an optional port and an optional channel, as the daemon writes them: no user or
     * password before the host, and no channel key after it.
     */
    private val LINK = Regex("""^(ircs?)://([a-z0-9.-]{1,253}|\[[0-9a-f:]{2,39}])(:[0-9]{1,5})?/([#&+!][^\s,?/#]{1,200})?$""")

    /** [link] as an IRC address an IRC app may be given, or null when it is not one. */
    fun safe(link: String): String? {
        val match = LINK.matchEntire(link.trim()) ?: return null
        val port = match.groupValues[3].drop(1)
        if (port.isNotEmpty() && port.toInt() !in 1..65535) return null
        return match.value
    }
}
