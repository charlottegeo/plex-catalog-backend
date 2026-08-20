ALTER TABLE public.servers
    ADD COLUMN status smallint NOT NULL DEFAULT 0,
    ADD COLUMN status_since timestamptz NULL;

UPDATE public.servers
SET status = CASE WHEN is_online THEN 0 ELSE 1 END;

ALTER TABLE public.servers
    DROP COLUMN is_online;

ALTER TABLE public.media_requests
    ADD COLUMN fulfilled_at timestamptz NULL;

UPDATE public.media_requests
SET fulfilled_at = updated_at
WHERE status = 'fulfilled' AND fulfilled_at IS NULL;
