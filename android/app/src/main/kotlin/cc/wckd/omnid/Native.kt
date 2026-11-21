package cc.wckd.omnid

/** The Rust daemon, `crates/omnid-android`. Every call blocks; never call from the main thread. */
internal object Native {
    init {
        System.loadLibrary("omnid_android")
    }

    /**
     * Starts the daemon if it is not running and returns its loopback port. Crawling starts
     * paused. Throws [RuntimeException] with the reason when it cannot start.
     */
    @JvmStatic
    external fun start(dataDir: String, token: String): Int

    /** Stops crawling, commits the index and shuts the daemon down. */
    @JvmStatic
    external fun stop()

    /** Pauses or resumes crawling; a non-empty [reason] is shown in the UI. */
    @JvmStatic
    external fun setPaused(paused: Boolean, reason: String)
}
