package com.lognara.client.ui.screens.data

import java.util.Locale
import org.junit.Assert.assertEquals
import org.junit.Test

class DataFormattingTest {
    @Test
    fun dataNumbersUseAtMostOneFractionalDigit() {
        val locale = Locale.forLanguageTag("ru-RU")

        assertEquals("115,7", formatDataNumber(115.741, locale))
        assertEquals("81", formatDataNumber(81.004, locale))
        assertEquals("100", formatDataNumber(100L, locale))
    }
}
