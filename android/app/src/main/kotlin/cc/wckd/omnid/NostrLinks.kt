package cc.wckd.omnid

/**
 * Nostr links handed to a Nostr app: the search page opens a Nostr result as a `nostr:` URI,
 * which such an app shows. Plain Kotlin, so the rule is unit-tested on the JVM.
 */
internal object NostrLinks {
    /** NIP-21: `nostr:` and a NIP-19 entity, in bech32's lower-case alphabet. Never `nsec`. */
    private val ENTITY = Regex("""^nostr:n(?:pub|ote|addr|event|profile)1[02-9ac-hj-np-z]{6,1000}$""")

    /** [link] as a `nostr:` URI a Nostr app may be given, or null when it is not one. */
    fun safe(link: String): String? = link.trim().lowercase().takeIf(ENTITY::matches)
}
