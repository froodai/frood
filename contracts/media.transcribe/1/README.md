# `media.transcribe@1`

Speech to text. The core calls it for captured audio (the `transcribe` job); the transcript
becomes the source's text.

`POST /v1/capabilities/media.transcribe@1`

Request:

```json
{
  "audio": { "content_type": "audio/ogg", "filename": "voice.oga", "data_base64": "T2dnUw…" },
  "languages": ["ru"]
}
```

- `audio.content_type` is what the client sent; the module converts formats its engine does
  not take (for example Telegram's OGG/Opus).
- `languages` is a hint (ISO 639-1 codes) or `null`.

Response:

```json
{ "text": "завтра в 6 часов новое занятие по вождению", "language": "ru", "segments": [], "model": "gpt-4o-mini-transcribe" }
```

- `text` may be empty: nothing was heard. The core then keeps the file and does not extract.
- `language`, `segments` (`[{ "start": 0.0, "end": 2.1, "text": "…" }]`) and `model` are
  optional.

Errors: 400 for audio the module cannot read (final); 5xx or `retryable: true` when the engine is
unavailable.
