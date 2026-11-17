package com.lognara.client.ui.screens.live

import com.lognara.client.data.LiveConnection
import com.lognara.client.data.LiveStreamListener
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

class LiveStreamControllerTest {
    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun stoppingDuringBackoffPreventsAnotherConnection() = runTest {
        var connections = 0
        var sessionChecks = 0
        val retryDelay = mutableListOf<Long>()
        val controller = LiveStreamController(
            scope = this,
            openStream = { listener ->
                connections++
                listener.onClosed()
                LiveConnection { }
            },
            checkSession = { sessionChecks++ },
            onStatus = { },
            onEvent = { _, _ -> },
            waitBeforeRetry = { milliseconds ->
                retryDelay += milliseconds
                delay(milliseconds)
            },
        )

        controller.start()
        testScheduler.runCurrent()
        assertEquals(1, connections)
        assertEquals(1, sessionChecks)
        assertEquals(listOf(1_000L), retryDelay)

        controller.stop()
        testScheduler.advanceTimeBy(60_000L)
        testScheduler.runCurrent()

        assertEquals(1, connections)
    }
}
