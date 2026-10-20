using System.Net;
using System.Net.Http.Headers;
using Microsoft.Extensions.Configuration;
using Serilog.Debugging;
using Serilog.Sinks.Http;

namespace Serilog.Sinks.Lognara;

internal sealed class AgentHttpClient : IHttpClient
{
    private readonly HttpClient client = new(new HttpClientHandler { AllowAutoRedirect = false })
    {
        Timeout = TimeSpan.FromSeconds(5)
    };

    public void Configure(IConfiguration configuration) { }

    public async Task<HttpResponseMessage> PostAsync(
        string requestUri, Stream contentStream, CancellationToken cancellationToken)
    {
        using var content = new BorrowedStreamContent(contentStream);
        content.Headers.ContentType = new MediaTypeHeaderValue("application/json") { CharSet = "utf-8" };
        try
        {
            using var response = await client.PostAsync(requestUri, content, cancellationToken).ConfigureAwait(false);
            // The sink consumes only the status. Dispose the network response here.
            return new HttpResponseMessage(response.StatusCode);
        }
        catch (Exception error) when (error is HttpRequestException ||
                                     error is OperationCanceledException && !cancellationToken.IsCancellationRequested)
        {
            // HttpSink retains its batch on an HTTP failure; a thrown transport error
            // can otherwise lose the batch already removed from its memory queue.
            SelfLog.WriteLine("Lognara agent request failed: {0}", error);
            return new HttpResponseMessage(HttpStatusCode.ServiceUnavailable);
        }
    }

    public void Dispose() => client.Dispose();

    // The HTTP sink still owns its stream/writer and disposes them after PostAsync.
    private sealed class BorrowedStreamContent(Stream source) : HttpContent
    {
        protected override Task SerializeToStreamAsync(Stream target, TransportContext? context) =>
            source.CopyToAsync(target);

        protected override Task SerializeToStreamAsync(
            Stream target, TransportContext? context, CancellationToken cancellationToken) =>
            source.CopyToAsync(target, cancellationToken);

        protected override bool TryComputeLength(out long length)
        {
            length = source.CanSeek ? source.Length - source.Position : 0;
            return source.CanSeek;
        }
    }
}
