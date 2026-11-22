package cc.wckd.omnid

import org.junit.Assert.assertEquals
import org.junit.Test

class NoticesTest {
    @Test
    fun cutsTheFileIntoHeadingsAndParagraphs() {
        val file = """
            Third-party software in OMNId 0.1.5: the OMNId Android app

            ======
            Rust crates
            ======

            MIT License (MIT)
            ------

            Used by: a 1.0, b
              2.0

                                 Apache License
                           Version 2.0, January 2004

            1. Definitions.
               "License" shall mean
        """.trimIndent()
        assertEquals(
            listOf(
                Notices.Paragraph("Third-party software in OMNId 0.1.5: the OMNId Android app", heading = false),
                Notices.Paragraph("Rust crates", heading = true),
                Notices.Paragraph("MIT License (MIT)", heading = true),
                Notices.Paragraph("Used by: a 1.0, b 2.0", heading = false),
                Notices.Paragraph("Apache License Version 2.0, January 2004", heading = false),
                Notices.Paragraph("1. Definitions. \"License\" shall mean", heading = false),
            ),
            Notices.paragraphs(file),
        )
    }

    @Test
    fun skipsEmptyBlocksAndBareRules() {
        assertEquals(emptyList<Notices.Paragraph>(), Notices.paragraphs("\n \n------\n\n\t\n"))
    }
}
