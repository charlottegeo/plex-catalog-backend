CREATE TABLE public.genres (
    id serial PRIMARY KEY,
    name text NOT NULL UNIQUE
);

CREATE TABLE public.item_genres (
    item_id text NOT NULL,
    server_id text NOT NULL,
    genre_id integer NOT NULL REFERENCES public.genres(id) ON DELETE CASCADE,
    PRIMARY KEY (item_id, server_id, genre_id),
    FOREIGN KEY (item_id, server_id) REFERENCES public.items(id, server_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_item_genres_genre ON public.item_genres USING btree (genre_id);
UPDATE public.items SET updated_at = NULL WHERE item_type IN ('movie', 'show');