# Outage and maintenance page

A self-contained page (`index.html`, no outside requests) that the reverse proxy in front of
the wiki serves while the engine does not answer, or while the operators take it down on
purpose. It keeps the address the reader asked for, checks the site by itself (after 10, 20,
40 seconds, then once a minute, or at once with "Check now") and opens it again when it is
back. A reader can also open the page they wanted in the Web Archive.

## What it says

Next to the page, `status.json` tells it what to say. Every field is optional; without the
file the page speaks of an outage.

```json
{
  "mode": "maintenance",
  "title": { "ru": "...", "en": "..." },
  "message": { "ru": "...", "en": "..." },
  "started": "2026-11-11T20:00:00Z",
  "eta": "2026-11-11T21:00:00Z",
  "status_url": "https://..."
}
```

- `mode`: `outage` (the default) or `maintenance`, which changes the heading and the wording.
- `started` and `eta`: how long it has been down and when it is expected back, with a bar.
- `status_url`: a public HTTPS link to where updates are posted. `STATUS_URL` inside the page
  is a fallback for a build that has its link baked in.

Edit the file on the proxy and the open pages pick it up at their next check.

## Proxy

An nginx example; the paths are placeholders.

```nginx
# The page and its status file, never cached.
location ^~ /__naw-outage/ {
    alias /srv/naw-outage/;
    add_header Cache-Control "no-store" always;
}

location / {
    # Planned maintenance: create the file to take the wiki down, remove it to bring it back.
    if (-f /srv/naw-outage/maintenance.on) {
        return 503;
    }
    proxy_pass http://wiki_backend;
    proxy_intercept_errors on;
    error_page 502 503 504 =503 /__naw-outage/index.html;
}
```

Keep `error_page` answering 503 so search engines know the outage is temporary.
