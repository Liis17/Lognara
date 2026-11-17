package com.lognara.client.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

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

    private fun assertPairFailure(status: Int, code: String, expected: PairError) {
        val result = parsePairResponse(status, """{"error":"$code"}""")

        assertEquals(PairResult.Failure(expected), result)
    }
}
