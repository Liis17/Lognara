package com.lognara.client.ui.screens.metrics

import com.lognara.client.data.SeriesMetric
import com.lognara.client.data.SeriesQuery
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MetricsModelTest {
    @Test
    fun seriesQuerySerializesOnlySupportedFields() {
        val metric = SeriesMetric(server = "node-1", service = "api", name = "requests", origin = "counter")
        val query = buildSeriesQuery(
            metric = metric,
            window = TimeWindow("2026-11-10T12:00:00Z", "2026-11-17T12:00:00Z"),
            range = DataRange.SevenDays,
            split = "labels",
            environment = "prod",
        )
        val body = apiJson.encodeToJsonElement(SeriesQuery.serializer(), query).jsonObject

        assertEquals(
            setOf("server", "origin", "service", "name", "from", "to", "step_seconds", "split", "environment"),
            body.keys,
        )
        assertEquals(3_600L, query.step_seconds)
        assertEquals("labels", query.split)
        assertEquals("prod", query.environment)
    }

    @Test
    fun chartScaleUsesFiniteValuesAndIgnoresNulls() {
        val scale = chartScale(listOf(null, 7.5, -2.0, Double.NaN, Double.POSITIVE_INFINITY))

        assertEquals(-2.0, scale?.minimum ?: 0.0, 0.0)
        assertEquals(7.5, scale?.maximum ?: 0.0, 0.0)
    }

    @Test
    fun chartScaleReturnsNullWhenThereAreNoFiniteValues() {
        assertNull(chartScale(listOf(null, Double.NaN, Double.NEGATIVE_INFINITY)))
    }

    @Test
    fun chartScaleExpandsConstantValuesToAvoidDivisionByZero() {
        val scale = chartScale(listOf(0.0, 0.0))

        assertEquals(-1.0, scale?.minimum ?: 0.0, 0.0)
        assertEquals(1.0, scale?.maximum ?: 0.0, 0.0)
    }
}
