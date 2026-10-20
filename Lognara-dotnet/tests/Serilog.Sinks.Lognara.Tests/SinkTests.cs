using System.Diagnostics;
using System.Globalization;
using System.Net;
using System.Text;
using System.Text.Json;
using System.Threading.Channels;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Logging;
using Serilog.Events;
using Serilog.Parsing;
using Xunit;

namespace Serilog.Sinks.Lognara.Tests;

public sealed class SinkTests
{
    [Fact]
    public void FormatsCanonicalFieldsAndStructuredProperties()
    {
        var timestamp = new DateTimeOffset(2026, 10, 17, 14, 30, 0, TimeSpan.FromHours(3));
        var trace = ActivityTraceId.CreateRandom();
        var span = ActivitySpanId.CreateRandom();
        var logEvent = new LogEvent(timestamp, LogEventLevel.Error,
            new InvalidOperationException("bad\n\"value\""),
            new MessageTemplateParser().Parse("Failed {Count} {Text}"),
            [new("Count", new ScalarValue(3)), new("Text", new ScalarValue("Привет\n\"мир\"")),
             new("Metrics", new StructureValue([new("Elapsed", new ScalarValue(12.5))])),
             new("Items", new SequenceValue([new ScalarValue(true), new ScalarValue(null)])),
             new("message", new ScalarValue("custom attribute"))], trace, span);
        using var writer = new StringWriter(CultureInfo.GetCultureInfo("ru-RU"));
        new LognaraJsonFormatter().Format(logEvent, writer);
        Assert.DoesNotContain('\n', writer.ToString());
        Assert.DoesNotContain('\r', writer.ToString());
        using var json = JsonDocument.Parse(writer.ToString());
        var root = json.RootElement;
        Assert.Equal(timestamp, root.GetProperty("timestamp").GetDateTimeOffset());
        Assert.Equal(TimeSpan.Zero, root.GetProperty("timestamp").GetDateTimeOffset().Offset);
        Assert.Equal("error", root.GetProperty("level").GetString());
        Assert.StartsWith("Failed 3", root.GetProperty("message").GetString());
        Assert.Equal(trace.ToHexString(), root.GetProperty("trace_id").GetString());
        Assert.Equal(span.ToHexString(), root.GetProperty("span_id").GetString());
        Assert.Contains("bad\n\"value\"", root.GetProperty("exception").GetString());
        var attributes = root.GetProperty("attributes");
        Assert.Equal(12.5, attributes.GetProperty("Metrics").GetProperty("Elapsed").GetDouble());
        Assert.Equal("Привет\n\"мир\"", attributes.GetProperty("Text").GetString());
        Assert.Equal(JsonValueKind.Null, attributes.GetProperty("Items")[1].ValueKind);
        Assert.Equal("custom attribute", attributes.GetProperty("message").GetString());
    }

    [Theory]
    [InlineData(LogEventLevel.Verbose, "trace")]
    [InlineData(LogEventLevel.Debug, "debug")]
    [InlineData(LogEventLevel.Information, "info")]
    [InlineData(LogEventLevel.Warning, "warn")]
    [InlineData(LogEventLevel.Error, "error")]
    [InlineData(LogEventLevel.Fatal, "fatal")]
    public void FormatsAllLevelsWithoutAbsentOptionalFields(LogEventLevel level, string expected)
    {
        var logEvent = new LogEvent(DateTimeOffset.UtcNow, level, null,
            new MessageTemplateParser().Parse("hello"), []);
        using var writer = new StringWriter();
        new LognaraJsonFormatter().Format(logEvent, writer);
        using var json = JsonDocument.Parse(writer.ToString());
        Assert.Equal(expected, json.RootElement.GetProperty("level").GetString());
        Assert.False(json.RootElement.TryGetProperty("trace_id", out _));
        Assert.False(json.RootElement.TryGetProperty("span_id", out _));
        Assert.False(json.RootElement.TryGetProperty("exception", out _));
    }

    [Theory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task SendsJsonArrayAndFlushesOnDispose(bool durable)
    {
        var directory = Path.Combine(Path.GetTempPath(), "lognara-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            await using var agent = await Agent.Start();
            using (var logger = new LoggerConfiguration()
                       .WriteTo.Lognara(agent.Url, period: TimeSpan.FromHours(1),
                           bufferBaseFileName: durable ? Path.Combine(directory, "buffer") : null).CreateLogger())
            {
                logger.Information("first {Value}", 42);
                logger.Warning("second");
            }
            var request = await agent.Next();
            Assert.StartsWith("application/json", request.ContentType);
            using var json = JsonDocument.Parse(request.Body);
            Assert.Equal(2, json.RootElement.GetArrayLength());
            Assert.Equal("first 42", json.RootElement[0].GetProperty("message").GetString());
            Assert.Equal("warn", json.RootElement[1].GetProperty("level").GetString());
        }
        finally { Directory.Delete(directory, recursive: true); }
    }

    [Theory]
    [InlineData(503)]
    [InlineData(400)]
    [InlineData(0)] // A broken connection must retain the dequeued batch, too.
    public async Task RetriesIdenticalBodyAfterFailure(int status)
    {
        await using var agent = await Agent.Start(status);
        using var logger = new LoggerConfiguration()
            .WriteTo.Lognara(agent.Url, period: TimeSpan.FromMilliseconds(20)).CreateLogger();
        logger.Error("keep me");
        var first = await agent.Next();
        var retry = await agent.Next();
        Assert.Equal(first.Body, retry.Body);
    }

    [Fact]
    public async Task KeepsBatchesBelowAgentLimitAndDropsOversizedEvents()
    {
        await using var agent = await Agent.Start();
        using (var logger = new LoggerConfiguration()
                   .WriteTo.Lognara(agent.Url, period: TimeSpan.FromHours(1)).CreateLogger())
        {
            logger.Information("{Text}", new string('я', 300_000));
            for (var i = 0; i < 12; i++)
                logger.Information("{Index} {Text}", i, new string('я', 40_000));
        }
        var count = 0;
        while (count < 12)
        {
            var request = await agent.Next();
            Assert.True(request.Body.Length < 2 * 1024 * 1024);
            using var json = JsonDocument.Parse(request.Body);
            count += json.RootElement.GetArrayLength();
            foreach (var item in json.RootElement.EnumerateArray())
                Assert.True(Encoding.UTF8.GetByteCount(item.GetRawText()) <= 256 * 1024);
        }
        Assert.Equal(12, count);
    }

    [Fact]
    public async Task MemoryQueueIsBoundedAndLevelFilterIsApplied()
    {
        await using var agent = await Agent.Start();
        using (var logger = new LoggerConfiguration().MinimumLevel.Verbose()
                   .WriteTo.Lognara(agent.Url, queueLimitBytes: 1024,
                       period: TimeSpan.FromHours(1), restrictedToMinimumLevel: LogEventLevel.Warning)
                   .CreateLogger())
        {
            logger.Information("filtered");
            for (var i = 0; i < 100; i++) logger.Warning("{Index} {Text}", i, new string('a', 200));
        }
        var request = await agent.Next();
        using var json = JsonDocument.Parse(request.Body);
        Assert.InRange(json.RootElement.GetArrayLength(), 1, 3);
        Assert.All(json.RootElement.EnumerateArray(), item =>
            Assert.Equal("warn", item.GetProperty("level").GetString()));
    }

    [Fact]
    public async Task DiskBufferResumesWithNewLogger()
    {
        var directory = Path.Combine(Path.GetTempPath(), "lognara-test-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(directory);
        try
        {
            await using var agent = await Agent.Start(503);
            var buffer = Path.Combine(directory, "buffer");
            using (var logger = new LoggerConfiguration().WriteTo.Lognara(agent.Url,
                       period: TimeSpan.FromHours(1), bufferBaseFileName: buffer).CreateLogger())
            {
                logger.Error("survive restart");
                logger.Warning("second buffered event");
                logger.Information("third buffered event");
            }
            var rejected = await agent.Next();
            using (var logger = new LoggerConfiguration().WriteTo.Lognara(agent.Url,
                       period: TimeSpan.FromHours(1), bufferBaseFileName: buffer).CreateLogger()) { }
            var recovered = await agent.Next();
            Assert.Equal(rejected.Body, recovered.Body);
            using var json = JsonDocument.Parse(recovered.Body);
            Assert.Equal(3, json.RootElement.GetArrayLength());
        }
        finally { Directory.Delete(directory, recursive: true); }
    }

    [Fact]
    public void RejectsInvalidConfigurationBeforeStartingSink()
    {
        Assert.Throws<ArgumentException>(() => new LoggerConfiguration().WriteTo.Lognara("file:///tmp/logs"));
        Assert.Throws<ArgumentOutOfRangeException>(() => new LoggerConfiguration().WriteTo.Lognara(queueLimitBytes: 0));
        Assert.Throws<ArgumentOutOfRangeException>(() => new LoggerConfiguration().WriteTo.Lognara(period: TimeSpan.Zero));
        Assert.Throws<ArgumentException>(() => new LoggerConfiguration().WriteTo.Lognara(bufferBaseFileName: " "));
    }

    private sealed record Request(byte[] Body, string ContentType);

    private sealed class Agent(WebApplication app, Channel<Request> requests) : IAsyncDisposable
    {
        public string Url => app.Urls.Single() + "/v1/logs";

        public async Task<Request> Next()
        {
            using var timeout = new CancellationTokenSource(TimeSpan.FromSeconds(15));
            return await requests.Reader.ReadAsync(timeout.Token);
        }

        public static async Task<Agent> Start(int firstStatus = 202)
        {
            var builder = WebApplication.CreateBuilder();
            builder.Logging.ClearProviders();
            builder.WebHost.UseUrls("http://127.0.0.1:0");
            var app = builder.Build();
            var requests = Channel.CreateUnbounded<Request>();
            var attempt = 0;
            app.MapPost("/v1/logs", async context =>
            {
                using var body = new MemoryStream();
                await context.Request.Body.CopyToAsync(body);
                var status = Interlocked.Increment(ref attempt) == 1 ? firstStatus : 202;
                if (status == 0) context.Abort();
                else context.Response.StatusCode = status;
                await requests.Writer.WriteAsync(new Request(body.ToArray(), context.Request.ContentType ?? ""));
            });
            await app.StartAsync();
            return new Agent(app, requests);
        }

        public async ValueTask DisposeAsync() => await app.DisposeAsync();
    }
}
