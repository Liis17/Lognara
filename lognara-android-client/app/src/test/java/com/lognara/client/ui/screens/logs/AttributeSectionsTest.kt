package com.lognara.client.ui.screens.logs

import java.util.Locale
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AttributeSectionsTest {
    private fun attributes(json: String): Map<String, kotlinx.serialization.json.JsonElement> =
        Json.parseToJsonElement(json).jsonObject

    @Test
    fun nestedObjectsBecomeNamedSections() {
        val values = attributes(
            """{"Metrics":{"Counters":{"net.rx":915426,"disk.read":192512},"Gauges":{"cpu.iowait_percent":1.81,"cpu.cores":4}}}""",
        )

        val sections = attributeSections(values, Locale.US)

        assertEquals(listOf("Counters", "Gauges"), sections.map { it.title })
        assertEquals(listOf("disk.read" to "192,512", "net.rx" to "915,426"), sections[0].rows)
        assertEquals(listOf("cpu.cores" to "4", "cpu.iowait_percent" to "1.81"), sections[1].rows)
        assertEquals("Metrics", attributeRootName(values))
    }

    @Test
    fun topLevelPrimitivesGoToDefaultSectionFirst() {
        val values = attributes("""{"user":"li","ok":true,"ctx":{"id":"7"},"list":[1,2]}""")

        val sections = attributeSections(values, Locale.US)

        assertNull(sections[0].title)
        assertEquals(listOf("list" to "[1,2]", "ok" to "true", "user" to "li"), sections[0].rows)
        assertEquals("ctx", sections[1].title)
        assertNull(attributeRootName(values))
        assertEquals(emptyList<AttributeSection>(), attributeSections(JsonObject(emptyMap()), Locale.US))
    }
}
