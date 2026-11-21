package cc.wckd.omnid

/** The Rust daemon, `crates/omnid-android`. Every call blocks; never call from the main thread. */
internal object Native {
    init {
        System.loadLibrary("omnid_android")
    }

    /**
     * Starts the daemon if it is not running and returns its loopback port. Crawling starts
     * paused. [hyphanetNode] is a Hyphanet node's FProxy URL ([HyphanetNode]) and [i2pRouter] an
     * I2P router's HTTP proxy ([I2pRouter]) elsewhere, each empty for only the one on this phone.
     * [crawlSpeed] is the ordinal of [Settings.CrawlSpeed]. Throws [RuntimeException] with the
     * reason when it cannot start.
     */
    @JvmStatic
    external fun start(dataDir: String, token: String, hyphanetNode: String, i2pRouter: String, crawlSpeed: Int): Int

    /** Stops crawling, commits the index and shuts the daemon down. */
    @JvmStatic
    external fun stop()

    /** Pauses or resumes crawling; a non-empty [reason] is shown in the UI. */
    @JvmStatic
    external fun setPaused(paused: Boolean, reason: String)

    /**
     * The port of the browser's SOCKS5 proxy on loopback, 0 when the daemon is not running. It
     * wants the daemon's token as its password and takes the site as the username.
     */
    @JvmStatic
    external fun browserProxyPort(): Int

    /** New Tor circuits for every site the browser visits from now on. */
    @JvmStatic
    external fun newIdentity()
}
