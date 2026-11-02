using System;
using System.Net.Http;
using System.Net.Http.Json;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace GraphMemory.Desktop;

/// <summary>
/// Minimal HTTP client for the local GraphMemory service.
/// Only loopback http endpoints are permitted; no remote servers are ever contacted.
/// </summary>
public sealed class ServiceClient : IDisposable
{
    private const int MaxErrorBodyLength = 512;

    private readonly HttpClient _http;

    public ServiceClient(string baseUrl)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(baseUrl);

        if (!Uri.TryCreate(baseUrl, UriKind.Absolute, out var uri))
        {
            throw new ArgumentException("Base URL must be an absolute URI.", nameof(baseUrl));
        }

        if (!string.Equals(uri.Scheme, Uri.UriSchemeHttp, StringComparison.OrdinalIgnoreCase))
        {
            throw new ArgumentException("Only the http scheme is supported.", nameof(baseUrl));
        }

        if (!uri.IsLoopback || !string.IsNullOrEmpty(uri.UserInfo))
        {
            throw new ArgumentException("Only loopback hosts are supported.", nameof(baseUrl));
        }

        var builder = new UriBuilder(uri)
        {
            Path = "/",
            Query = string.Empty,
            Fragment = string.Empty,
        };

        _http = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false })
        {
            BaseAddress = builder.Uri,
            Timeout = TimeSpan.FromSeconds(10),
        };
    }

    public Task<JsonNode> GetAsync(string path) => SendAsync(HttpMethod.Get, path, body: null);

    public Task<JsonNode> PostAsync(string path, object body) => SendAsync(HttpMethod.Post, path, body);

    private async Task<JsonNode> SendAsync(HttpMethod method, string path, object? body)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(path);

        if (Uri.TryCreate(path, UriKind.Absolute, out _) || path.StartsWith("//"))
            throw new ArgumentException("API path must be relative.", nameof(path));
        using var request = new HttpRequestMessage(method, path.TrimStart('/'));
        if (body is not null)
        {
            request.Content = new StringContent(JsonSerializer.Serialize(body), System.Text.Encoding.UTF8, "application/json");
        }

        using var response = await _http.SendAsync(request).ConfigureAwait(false);
        var payload = await response.Content.ReadAsStringAsync().ConfigureAwait(false);

        if (!response.IsSuccessStatusCode)
        {
            throw new InvalidOperationException(DescribeFailure(response, payload));
        }

        return JsonNode.Parse(payload) ?? throw new InvalidOperationException(
            $"Server returned an empty response body (HTTP {(int)response.StatusCode}).");
    }

    private static string DescribeFailure(HttpResponseMessage response, string payload)
    {
        var status = $"HTTP {(int)response.StatusCode} ({response.ReasonPhrase})";
        var detail = TryReadErrorMessage(payload);
        return detail is null ? $"Request failed: {status}." : $"Request failed: {status}: {detail}";
    }

    private static string? TryReadErrorMessage(string payload)
    {
        if (string.IsNullOrWhiteSpace(payload) || payload.Length > MaxErrorBodyLength)
        {
            return null;
        }

        try
        {
            return JsonNode.Parse(payload)?["error"]?.GetValue<string>();
        }
        catch (Exception ex) when (ex is JsonException or InvalidOperationException or FormatException)
        {
            return null;
        }
    }

    public void Dispose() => _http.Dispose();
}