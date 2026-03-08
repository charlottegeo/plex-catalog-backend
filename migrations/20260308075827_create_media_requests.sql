CREATE TABLE public.media_requests (
    id SERIAL PRIMARY KEY,
    username TEXT NOT NULL,
    guid TEXT NOT NULL,
    title TEXT NOT NULL,
    item_type TEXT NOT NULL,
    requested_seasons INTEGER[],
    requested_resolution TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX media_requests_pending_username_guid_idx
    ON public.media_requests (username, guid)
    WHERE status = 'pending';
