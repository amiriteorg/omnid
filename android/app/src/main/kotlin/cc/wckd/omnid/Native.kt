package cc.wckd.omnid

/** The Rust daemon, `crates/omnid-android`. Every call blocks; never call from the main thread. */
internal object Native {
    init {
        System.loadLibrary("omnid_android")
    }

    /**
     * Starts the daemon if it is not running and returns its loopback port. Crawling starts
     * paused. [options] is a JSON object of the settings it takes at start (`omnid-android`'s
     * `Options`): `hyphanetNode` ([HyphanetNode]) and `i2pRouter` ([I2pRouter]), each empty for
     * only the one on this phone; `ipfsGateway` ([IpfsGateway]), empty for none; `nostr`, whether
     * to read Nostr relays; `crawlSpeed`, the ordinal of [Settings.CrawlSpeed]; and
     * `appProxy`, `{port, password}`, when the proxy for other apps is on. Throws
     * [RuntimeException] with the reason when it cannot start.
     */
    @JvmStatic
    external fun start(dataDir: String, token: String, options: String): Int

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

    /** The port of the proxy for other apps, 0 when it is off or another app holds the port. */
    @JvmStatic
    external fun appProxyPort(): Int

    /**
     * Starts the daemon downloading [url] into [fd], which it takes over and closes: in up to
     * [connections] parallel ranges, each on a Tor circuit of its own, over the browser's route for
     * [site], asking with [userAgent]. [length], [etag] and [lastModified] are what the browser saw
     * (-1 or "" for none); an answer that differs ends in the changed state. Returns its id.
     */
    @JvmStatic
    external fun download(url: String, site: String, userAgent: String, fd: Int, connections: Int, length: Long, etag: String, lastModified: String): Long

    /**
     * A download's bytes done, its size (-1 while unknown), the connections working, and its
     * state: 0 probing, 1 running, 2 done, 3 failed, 4 changed (not the file the browser saw),
     * 5 cancelled.
     */
    @JvmStatic
    external fun downloadState(id: Long): LongArray

    /** Why a download failed or changed. */
    @JvmStatic
    external fun downloadError(id: Long): String

    @JvmStatic
    external fun cancelDownload(id: Long)

    /** Lets the daemon forget a download that is over. */
    @JvmStatic
    external fun forgetDownload(id: Long)

    /** New Tor circuits for every site the browser visits from now on. */
    @JvmStatic
    external fun newIdentity()
}
