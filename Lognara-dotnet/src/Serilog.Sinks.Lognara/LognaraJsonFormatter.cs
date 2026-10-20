using System.Globalization;
using Serilog.Events;
using Serilog.Formatting;
using Serilog.Formatting.Json;

namespace Serilog.Sinks.Lognara;

internal sealed class LognaraJsonFormatter : ITextFormatter
{
    private readonly JsonValueFormatter values = new(typeTagName: null);

    public void Format(LogEvent logEvent, TextWriter output)
    {
        output.Write('{');
        WriteString("timestamp", logEvent.Timestamp.ToUniversalTime().ToString("O", CultureInfo.InvariantCulture), output);
        output.Write(',');
        WriteString("level", logEvent.Level switch
        {
            LogEventLevel.Verbose => "trace",
            LogEventLevel.Information => "info",
            LogEventLevel.Warning => "warn",
            _ => logEvent.Level.ToString().ToLowerInvariant()
        }, output);
        output.Write(',');
        WriteString("message", logEvent.RenderMessage(CultureInfo.InvariantCulture), output);
        if (logEvent.TraceId is { } traceId)
        {
            output.Write(',');
            WriteString("trace_id", traceId.ToHexString(), output);
        }
        if (logEvent.SpanId is { } spanId)
        {
            output.Write(',');
            WriteString("span_id", spanId.ToHexString(), output);
        }
        if (logEvent.Exception is { } exception)
        {
            output.Write(',');
            WriteString("exception", exception.ToString(), output);
        }
        output.Write(",\"attributes\":{");
        var first = true;
        foreach (var property in logEvent.Properties)
        {
            if (!first) output.Write(',');
            first = false;
            JsonValueFormatter.WriteQuotedJsonString(property.Key, output);
            output.Write(':');
            values.Format(property.Value, output);
        }
        // The durable sink owns record delimiters; its newline is platform-specific.
        output.Write("}}");
    }

    private static void WriteString(string name, string value, TextWriter output)
    {
        JsonValueFormatter.WriteQuotedJsonString(name, output);
        output.Write(':');
        JsonValueFormatter.WriteQuotedJsonString(value, output);
    }
}
