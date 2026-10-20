using Serilog.Configuration;
using Serilog.Core;
using Serilog.Events;
using Serilog.Sinks.Http.BatchFormatters;
using Serilog.Sinks.Lognara;

namespace Serilog;

/// <summary>Configures delivery to the Lognara agent.</summary>
public static class LognaraLoggerConfigurationExtensions
{
    /// <summary>
    /// Sends JSON batches to the agent. The default queue is in memory; supplying
    /// bufferBaseFileName enables a disk buffer (four files of approximately 16 MiB).
    /// Oversized events and events exceeding queue capacity can be dropped; see SelfLog.
    /// </summary>
    public static LoggerConfiguration Lognara(
        this LoggerSinkConfiguration sinkConfiguration,
        string requestUri = "http://127.0.0.1:7400/v1/logs",
        long queueLimitBytes = 16 * 1024 * 1024,
        TimeSpan? period = null,
        string? bufferBaseFileName = null,
        LogEventLevel restrictedToMinimumLevel = LevelAlias.Minimum,
        LoggingLevelSwitch? levelSwitch = null)
    {
        ArgumentNullException.ThrowIfNull(sinkConfiguration);
        if (!Uri.TryCreate(requestUri, UriKind.Absolute, out var uri) ||
            (uri.Scheme != Uri.UriSchemeHttp && uri.Scheme != Uri.UriSchemeHttps))
            throw new ArgumentException("An absolute HTTP(S) agent URL is required.", nameof(requestUri));
        ArgumentOutOfRangeException.ThrowIfNegativeOrZero(queueLimitBytes);
        var interval = period ?? TimeSpan.FromSeconds(1);
        if (interval <= TimeSpan.Zero || interval.TotalMilliseconds > int.MaxValue)
            throw new ArgumentOutOfRangeException(nameof(period));
        if (bufferBaseFileName is not null && string.IsNullOrWhiteSpace(bufferBaseFileName))
            throw new ArgumentException("The disk buffer path must not be empty.", nameof(bufferBaseFileName));

        // Leave room for array delimiters below the agent's 2 MiB HTTP body limit.
        const long eventBytes = 256 * 1024;
        const long batchBytes = 1024 * 1024;
        const int batchCount = 100;
        if (bufferBaseFileName is not null)
            return sinkConfiguration.DurableHttpUsingFileSizeRolledBuffers(
                requestUri: requestUri,
                bufferBaseFileName: bufferBaseFileName,
                bufferFileSizeLimitBytes: 16 * 1024 * 1024,
                retainedBufferFileCountLimit: 4,
                logEventLimitBytes: eventBytes,
                logEventsInBatchLimit: batchCount,
                batchSizeLimitBytes: batchBytes,
                period: interval,
                textFormatter: new LognaraJsonFormatter(),
                batchFormatter: new ArrayBatchFormatter(),
                httpClient: new AgentHttpClient(),
                restrictedToMinimumLevel: restrictedToMinimumLevel,
                levelSwitch: levelSwitch);

        return sinkConfiguration.Http(
            requestUri: requestUri,
            queueLimitBytes: queueLimitBytes,
            logEventLimitBytes: eventBytes,
            logEventsInBatchLimit: batchCount,
            batchSizeLimitBytes: batchBytes,
            period: interval,
            textFormatter: new LognaraJsonFormatter(),
            batchFormatter: new ArrayBatchFormatter(),
            httpClient: new AgentHttpClient(),
            restrictedToMinimumLevel: restrictedToMinimumLevel,
            levelSwitch: levelSwitch);
    }
}
