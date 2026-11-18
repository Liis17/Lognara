package com.lognara.client.ui.screens.issues

import com.lognara.client.data.GroupRequest
import com.lognara.client.data.IssueDetailsRequest
import com.lognara.client.data.IssueSearchRequest
import com.lognara.client.data.IssueStatus
import com.lognara.client.data.apiJson
import com.lognara.client.ui.screens.data.DataRange
import com.lognara.client.ui.screens.data.TimeWindow
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Test

class IssueRequestTest {
    private val window = TimeWindow("2026-11-16T12:00:00Z", "2026-11-17T12:00:00Z")

    @Test
    fun issueSearchAndServicesRequestsUseDockFields() {
        val search = buildIssueSearchRequest(window, DataRange.TwentyFourHours, IssueStatus.REGRESSED, "api")
        val services = buildIssueServicesRequest(window)
        val searchJson = apiJson.encodeToJsonElement(IssueSearchRequest.serializer(), search).jsonObject
        val servicesJson = apiJson.encodeToJsonElement(GroupRequest.serializer(), services).jsonObject

        assertEquals(setOf("from", "to", "filters", "interval_seconds", "limit", "status"), searchJson.keys)
        assertEquals("regressed", searchJson["status"]?.toString()?.trim('"'))
        assertEquals(mapOf("service" to listOf("api")), search.filters)
        assertEquals(setOf("from", "to", "filters", "group_by", "limit"), servicesJson.keys)
        assertEquals(mapOf("level" to listOf("error", "fatal")), services.filters)
        assertEquals(listOf("service"), services.group_by)
    }

    @Test
    fun issueDetailsRequestUsesOnlyDockFields() {
        val request = IssueDetailsRequest(
            from = window.from,
            to = window.to,
            filters = issueFilters("api"),
            interval_seconds = issueIntervalSeconds(DataRange.TwentyFourHours),
        )
        val body = apiJson.encodeToJsonElement(IssueDetailsRequest.serializer(), request).jsonObject

        assertEquals(setOf("from", "to", "filters", "interval_seconds"), body.keys)
        assertEquals(3_600L, request.interval_seconds)
    }
}
