CREATE INDEX IF NOT EXISTS idx_items_server_library_type ON public.items USING btree (server_id, library_id, item_type);

CREATE INDEX IF NOT EXISTS idx_items_top_level ON public.items USING btree (item_type, id, server_id)
    WHERE item_type IN ('movie', 'show');