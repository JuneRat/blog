# Image header fixtures

These 1×1 red pixels were generated locally with Pillow (PNG, JPEG, GIF,
WebP lossless and lossy) and fully decoded again to verify their dimensions.
They are complete image files rather than handcrafted headers.

The lossy WebP regression covers the three-byte VP8 frame tag preceding the
key-frame start code: [RFC 6386 §9.1](https://datatracker.ietf.org/doc/html/rfc6386#section-9.1).
