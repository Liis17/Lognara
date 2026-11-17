package com.lognara.client.data

import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Test

class PairPayloadTest {
    @Test
    fun decodesPayloadWithoutPaddingAndRemovesTrailingSlash() {
        val decoded = decodePairPayload(payload(url = "https://dock.example.com/"), allowInsecureLocalhost = false)

        assertEquals("https://dock.example.com", decoded.url)
        assertEquals("AQIDBA", decoded.code)
    }

    @Test
    fun acceptsPaddedPayloadAndCode() {
        val encoded = Base64.getUrlEncoder().encodeToString(
            """{"v":1,"url":"https://dock.example.com","code":"AQIDBA=="}""".toByteArray(),
        )

        assertEquals("AQIDBA==", decodePairPayload(encoded, false).code)
    }

    @Test
    fun rejectsInvalidBase64() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload("%%%", false)
        }

        assertEquals(PayloadIssue.InvalidBase64, exception.issue)
    }

    @Test
    fun rejectsUnsupportedVersion() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(version = 2), false)
        }

        assertEquals(PayloadIssue.UnsupportedVersion, exception.issue)
    }

    @Test
    fun rejectsHttpForRemoteHosts() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(url = "http://dock.example.com"), allowInsecureLocalhost = true)
        }

        assertEquals(PayloadIssue.InsecureUrl, exception.issue)
    }

    @Test
    fun allowsHttpLocalhostOnlyInDebugBuild() {
        assertEquals(
            "http://localhost:8080",
            decodePairPayload(payload(url = "http://localhost:8080/"), allowInsecureLocalhost = true).url,
        )
        assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(url = "http://127.0.0.1:8080"), allowInsecureLocalhost = false)
        }
    }

    @Test
    fun rejectsPortZero() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(url = "https://dock.example.com:0"), false)
        }

        assertEquals(PayloadIssue.InvalidUrl, exception.issue)
    }

    @Test
    fun rejectsPortAboveMaximum() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(url = "https://dock.example.com:99999"), false)
        }

        assertEquals(PayloadIssue.InvalidUrl, exception.issue)
    }

    @Test
    fun pairPayloadToStringMasksCode() {
        assertFalse(PairPayload(1, "https://dock.example.com", "pair-code").toString().contains("pair-code"))
    }

    @Test
    fun rejectsEmptyCode() {
        val exception = assertThrows(PairPayloadException::class.java) {
            decodePairPayload(payload(code = ""), false)
        }

        assertEquals(PayloadIssue.InvalidCode, exception.issue)
    }

    @Test
    fun deviceNameIsTrimmedAndCappedAt64Characters() {
        assertEquals("Google Pixel 8 Pro", buildDeviceName(" Google ", " Pixel 8 Pro "))
        assertEquals(64, buildDeviceName("M".repeat(50), "D".repeat(50)).length)
        val unicodeName = buildDeviceName("🙂".repeat(70), "")
        assertEquals(64, unicodeName.codePointCount(0, unicodeName.length))
        assertEquals(128, unicodeName.length)
        assertEquals("Pixel", buildDeviceName("", "Pixel"))
    }

    private fun payload(
        url: String = "https://dock.example.com/",
        code: String = "AQIDBA",
        version: Int = 1,
    ): String {
        val json = """{"v":$version,"url":"$url","code":"$code"}"""
        return Base64.getUrlEncoder().withoutPadding().encodeToString(json.toByteArray())
    }
}
