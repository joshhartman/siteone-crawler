# Test certificate

`localhost.key.der` is a **throwaway private key for local tests only**. It protects nothing: it was
generated for these tests, is published with the source, and must never be used anywhere else.

`localhost.cert.der` is its self-signed certificate (`CN=localhost`, SAN `localhost`, `127.0.0.1`,
`::1`, valid until 2126). The tests reach the server only on loopback, with
`--accept-invalid-certs`. Made once with the `openssl` of Git for Windows:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -keyout key.pem -out cert.pem -days 36500 -subj "/CN=localhost" \
  -addext "subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1"
openssl x509 -in cert.pem -outform der -out localhost.cert.der
openssl pkcs8 -topk8 -nocrypt -in key.pem -outform der -out localhost.key.der
```
