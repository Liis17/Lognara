package com.lognara.client.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.jsonObject

class ApiModelsTest {
    @Test
    fun parsesPairSuccessAndIgnoresUnknownFields() {
        val result = parsePairResponse(
            201,
            """{"token":"lgd_test","device_id":"device-1","username":"alice","extra":true}""",
        )

        assertTrue(result is PairResult.Success)
        assertEquals("alice", (result as PairResult.Success).response.username)
    }

    @Test
    fun mapsPairErrorResponsesToFriendlyCategories() {
        assertPairFailure(400, "invalid_request", PairError.InvalidRequest)
        assertPairFailure(401, "invalid_code", PairError.InvalidCode)
        assertPairFailure(409, "device_limit", PairError.DeviceLimit)
        assertPairFailure(429, "too_many_attempts", PairError.TooManyAttempts)
        assertPairFailure(404, "not_found", PairError.OutdatedDock)
    }

    @Test
    fun malformedPairResponseIsRejected() {
        val result = parsePairResponse(201, "not json")

        assertEquals(PairResult.Failure(PairError.InvalidResponse), result)
    }

    @Test
    fun mapsRedirectResponsesToFriendlyCategory() {
        assertPairFailure(302, "", PairError.Redirect)
    }

    @Test
    fun sensitiveModelToStringsMaskSecrets() {
        val values = listOf(
            PairRequest("pair-code", "device").toString(),
            PairResponse("device-token", "device-1", "alice").toString(),
            DeviceSession("https://dock.example.com", "device-token", "device-1", "alice").toString(),
            EncryptedToken(byteArrayOf(1, 2), "encrypted-secret".toByteArray()).toString(),
        )

        assertFalse(values.any { it.contains("pair-code") })
        assertFalse(values.any { it.contains("device-token") })
        assertFalse(values.any { it.contains("encrypted-secret") })
    }

    @Test
    fun decodesDockLogSummaryAndGroupFixtures() {
        val summary = apiJson.decodeFromString<SummaryResponse>(
            """{"as_of":"2026-11-17T12:00:00Z","total":1,"buckets":[{"timestamp":"2026-11-17T11:59:00Z","count":1,"levels":{"trace":0,"debug":0,"info":0,"warn":0,"error":1,"fatal":0,"unknown":0}}],"levels":{"trace":0,"debug":0,"info":0,"warn":0,"error":1,"fatal":0,"unknown":0},"error_services":[{"service":"api","count":1}],"duration":{"p95_ms":null,"measured":0,"coverage":0.0,"method":"tdigest"},"actions":[],"page":{"events":[{"id":"event-1","timestamp":"2026-11-17T11:59:30Z","ingested_at":"2026-11-17T11:59:31Z","environment":null,"server":"node-1","backend":"http","service":"api","service_instance":null,"action":null,"level":"error","message":"failed","trace_id":"0123456789abcdef0123456789abcdef","span_id":null,"parent_span_id":null,"request_id":null,"attributes":{"attempt":2}}],"next_cursor":"cursor-1","as_of":"2026-11-17T12:00:00Z"}}""",
        )
        val groups = apiJson.decodeFromString<GroupResponse>(
            """{"groups":[{"values":{"service":"api"},"count":1}],"as_of":"2026-11-17T12:00:00Z"}""",
        )

        assertEquals(1, summary.total)
        assertEquals(LogLevel.ERROR, summary.page?.events?.single()?.level)
        assertEquals(1, summary.buckets.single().levels.error)
        assertEquals("api", groups.groups.single().values["service"])
    }

    @Test
    fun decodesDockIssueInfrastructureAndMetricFixtures() {
        val issue = apiJson.decodeFromString<IssueResponse>(
            """{"algorithm_version":1,"as_of":"2026-11-17T12:00:00Z","watermark":3,"core_watermark":3,"history_from":null,"history_complete":true,"counts":{"open":1,"new":0,"regressed":0,"resolved":0},"status_events":0,"issues":[{"fingerprint":"f1_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","environment":null,"service":"api","action":null,"exception_type":"Failure","frames":["app.rs:42"],"pattern_id":"p1","template":"failed","parts":[],"exact":false,"count":2,"servers":1,"is_new":false,"first_seen":"2026-11-17T11:00:00Z","last_seen":"2026-11-17T11:59:00Z","buckets":[{"timestamp":"2026-11-17T11:59:00Z","count":2}],"state":{"status":"open","resolved_at":null,"resolved_by":null}}]}""",
        )
        val infrastructure = apiJson.decodeFromString<InfrastructureResponse>(
            """{"as_of":"2026-11-17T12:00:00Z","thresholds":{"heartbeat_ttl_seconds":60,"idle_seconds":300,"spool_warn_percent":80},"core":{"status":"online","version":"1.2.3","visibility_lag_ms":5,"wal_bytes":100,"wal_limit_bytes":1000},"storage":null,"dock":{"version":"1.2.3","sse_clients":1,"sessions":2},"registry":true,"nodes":[{"kind":"relay","id":1,"name":"relay-1","relay_id":null,"relay":null,"status":"online","reason":null,"heartbeat_at":null,"last_event_at":null,"version":"1.2.3","spool":null,"delivery_failures":0,"events_per_second":1.5,"source":null}]}""",
        )
        val metric = apiJson.decodeFromString<SeriesMetricsResponse>(
            """{"metrics":[{"server":"node-1","service":"api","name":"requests","origin":"counter","first_seen":null,"last_seen":null,"definition":null,"environments":[] }]}""",
        )
        val series = apiJson.decodeFromString<SeriesReply>(
            """{"kind":"delta","aggregation":"sum","unit":"count","buckets":["2026-11-17T12:00:00Z"],"series":[{"label":"api","values":[2.0]}]}""",
        )

        assertEquals(2, issue.issues.single().count)
        assertEquals("online", infrastructure.core.status)
        assertEquals("requests", metric.metrics.single().name)
        assertEquals(2.0, series.series.single().values.single() ?: 0.0, 0.0)
    }

    @Test
    fun decodesDockIssueDetailsFixture() {
        val details = apiJson.decodeFromString<IssueDetailsResponse>(
            """{"as_of":"2026-11-17T12:00:00Z","issue":{"fingerprint":"f1_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","environment":null,"service":"api","action":null,"exception_type":"Failure","frames":["app.rs:42"],"pattern_id":"p1","template":"failed","parts":[],"exact":false,"count":2,"servers":1,"is_new":false,"first_seen":"2026-11-17T11:00:00Z","last_seen":"2026-11-17T11:59:00Z","buckets":[]},"servers":[{"value":"node-1","count":2}],"other_servers":1,"examples":[{"id":"event-1","timestamp":"2026-11-17T11:59:00Z","ingested_at":"2026-11-17T11:59:01Z","server":"node-1","backend":"http","service":"api","level":"error","message":"failed"}],"state":{"status":"regressed","resolved_at":null,"resolved_by":null},"history":[{"action":"regressed","actor":null,"at":"2026-11-17T11:59:00Z","event_at":"2026-11-17T11:58:59Z"}]}""",
        )

        assertEquals("node-1", details.servers.single().value)
        assertEquals("event-1", details.examples.single().id)
        assertEquals("regressed", details.state.status)
        assertEquals("regressed", details.history.single().action)
    }

    @Test
    fun summaryRequestSerializesOnlyDockFields() {
        val request = SummaryRequest(
            from = "2026-11-17T11:00:00Z",
            to = "2026-11-17T12:00:00Z",
            filters = mapOf("level" to listOf("error")),
            text = TextQuery("timeout", "all"),
            interval_seconds = 60,
            search_limit = 50,
        )
        val body = apiJson.encodeToJsonElement(SummaryRequest.serializer(), request).jsonObject

        assertEquals(setOf("from", "to", "filters", "text", "interval_seconds", "search_limit"), body.keys)
    }

    private fun assertPairFailure(status: Int, code: String, expected: PairError) {
        val result = parsePairResponse(status, """{"error":"$code"}""")

        assertEquals(PairResult.Failure(expected), result)
    }
}
