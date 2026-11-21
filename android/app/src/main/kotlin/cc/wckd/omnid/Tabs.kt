package cc.wckd.omnid

/**
 * The browser's open tabs, in order, and which one is shown. Plain Kotlin, so the rules are
 * unit-tested on the JVM; the activity keeps a GeckoSession in each.
 */
internal class TabList<T>(private val max: Int) {
    private val items = mutableListOf<T>()

    /** The shown tab's position, or -1 when there are none. */
    var current: Int = -1
        private set

    val all: List<T> get() = items
    val size: Int get() = items.size
    val shown: T? get() = items.getOrNull(current)
    val isFull: Boolean get() = items.size >= max

    /**
     * Adds a tab right after the shown one, the way a link opened in a new tab sits next to its
     * page, and shows it unless [background]. Returns false when there is no room.
     */
    fun add(item: T, background: Boolean = false): Boolean {
        if (isFull) return false
        val at = if (current < 0) items.size else current + 1
        items.add(at, item)
        if (!background || current < 0) current = at
        return true
    }

    fun select(item: T) {
        val at = items.indexOf(item)
        if (at >= 0) current = at
    }

    /**
     * Closes [item]. When it was shown, the tab after it is shown instead, or the one before it
     * at the end. Returns the tab shown now, null when none are left.
     */
    fun close(item: T): T? {
        val at = items.indexOf(item)
        if (at < 0) return shown
        items.removeAt(at)
        current = when {
            items.isEmpty() -> -1
            at < current -> current - 1
            at == current -> at.coerceAtMost(items.size - 1)
            else -> current
        }
        return shown
    }

    fun clear() {
        items.clear()
        current = -1
    }
}
