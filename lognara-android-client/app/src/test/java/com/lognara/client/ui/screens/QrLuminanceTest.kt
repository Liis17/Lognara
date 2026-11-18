package com.lognara.client.ui.screens

import com.google.zxing.BarcodeFormat
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.qrcode.QRCodeWriter
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test

class QrLuminanceTest {
    private val pixels = byteArrayOf(1, 2, 3, 4, 5, 6)

    @Test
    fun rotatesClockwise90Degrees() {
        val rotated = rotateLuminance(pixels, width = 2, height = 3, rotationDegrees = 90)

        assertEquals(3, rotated.width)
        assertEquals(2, rotated.height)
        assertArrayEquals(byteArrayOf(5, 3, 1, 6, 4, 2), rotated.pixels)
    }

    @Test
    fun rotates180Degrees() {
        val rotated = rotateLuminance(pixels, width = 2, height = 3, rotationDegrees = 180)

        assertEquals(2, rotated.width)
        assertEquals(3, rotated.height)
        assertArrayEquals(byteArrayOf(6, 5, 4, 3, 2, 1), rotated.pixels)
    }

    @Test
    fun rotatesClockwise270Degrees() {
        val rotated = rotateLuminance(pixels, width = 2, height = 3, rotationDegrees = 270)

        assertEquals(3, rotated.width)
        assertEquals(2, rotated.height)
        assertArrayEquals(byteArrayOf(2, 4, 6, 1, 3, 5), rotated.pixels)
    }

    @Test
    fun decodesQrFromPlanarYuvSourceWithoutRotatingTheSource() {
        val matrix = QRCodeWriter().encode("lognara-test", BarcodeFormat.QR_CODE, 128, 128)
        val luminance = ByteArray(matrix.width * matrix.height) { index ->
            if (matrix[index % matrix.width, index / matrix.width]) 0 else 0xFF.toByte()
        }
        val source = PlanarYUVLuminanceSource(
            luminance,
            matrix.width,
            matrix.height,
            0,
            0,
            matrix.width,
            matrix.height,
            false,
        )

        assertEquals("lognara-test", decodeQrFromSource(source))
    }
}
