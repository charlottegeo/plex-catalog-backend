CREATE FUNCTION public.items_fts_trigger() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    new.fts_document :=
        to_tsvector('simple', regexp_replace(coalesce(new.title, ''), '\.', '', 'g'));
    return new;
END
$$;

CREATE TABLE public.items (
    id text NOT NULL,
    library_id text NOT NULL,
    server_id text NOT NULL,
    parent_id text,
    title text NOT NULL,
    summary text,
    item_type text NOT NULL,
    year smallint,
    thumb_path text,
    art_path text,
    last_seen timestamp with time zone DEFAULT now() NOT NULL,
    fts_document tsvector,
    guid character varying(255),
    index integer,
    leaf_count integer,
    updated_at bigint,
    content_rating text,
    duration bigint,
    originally_available_at date,
    studio text
);

CREATE TABLE public.libraries (
    id text NOT NULL,
    server_id text NOT NULL,
    title text NOT NULL,
    library_type text NOT NULL,
    last_seen timestamp with time zone DEFAULT now() NOT NULL
);

CREATE TABLE public.media_parts (
    id bigint NOT NULL,
    item_id text NOT NULL,
    video_resolution text,
    duration_ms bigint,
    file_path text,
    last_seen timestamp with time zone DEFAULT now() NOT NULL,
    server_id character varying(255) NOT NULL
);

CREATE TABLE public.servers (
    id text NOT NULL,
    name text NOT NULL,
    access_token text NOT NULL,
    connection_uri text NOT NULL,
    last_seen timestamp with time zone DEFAULT now() NOT NULL,
    is_online boolean DEFAULT true NOT NULL
);

CREATE TABLE public.streams (
    id bigint NOT NULL,
    media_part_id bigint NOT NULL,
    stream_type smallint NOT NULL,
    codec text,
    language text,
    language_code text,
    format text,
    last_seen timestamp with time zone DEFAULT now() NOT NULL,
    server_id character varying(255) NOT NULL
);

CREATE TABLE public.sync_metadata (
    id integer DEFAULT 1 NOT NULL,
    last_updated timestamp with time zone,
    CONSTRAINT single_row CHECK ((id = 1))
);

ALTER TABLE ONLY public.items
    ADD CONSTRAINT items_pkey PRIMARY KEY (id, server_id);

ALTER TABLE ONLY public.libraries
    ADD CONSTRAINT libraries_pkey PRIMARY KEY (id, server_id);

ALTER TABLE ONLY public.media_parts
    ADD CONSTRAINT media_parts_pkey PRIMARY KEY (id, server_id);

ALTER TABLE ONLY public.servers
    ADD CONSTRAINT servers_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.streams
    ADD CONSTRAINT streams_pkey PRIMARY KEY (id, server_id);

ALTER TABLE ONLY public.sync_metadata
    ADD CONSTRAINT sync_metadata_pkey PRIMARY KEY (id);

CREATE INDEX idx_items_guid ON public.items USING btree (guid);
CREATE INDEX idx_items_index ON public.items USING btree (index);
CREATE INDEX idx_items_last_seen ON public.items USING btree (last_seen);
CREATE INDEX idx_items_library_id ON public.items USING btree (library_id);
CREATE INDEX idx_items_originally_available_at ON public.items USING btree (originally_available_at);
CREATE INDEX idx_items_parent_id ON public.items USING btree (parent_id);
CREATE INDEX idx_libraries_last_seen ON public.libraries USING btree (last_seen);
CREATE INDEX idx_libraries_server_id ON public.libraries USING btree (server_id);
CREATE INDEX idx_media_parts_item_id ON public.media_parts USING btree (item_id);
CREATE INDEX idx_media_parts_last_seen ON public.media_parts USING btree (last_seen);
CREATE INDEX idx_streams_last_seen ON public.streams USING btree (last_seen);
CREATE INDEX idx_streams_media_part_id ON public.streams USING btree (media_part_id);
CREATE INDEX items_fts_document_idx ON public.items USING gin (fts_document);

CREATE TRIGGER items_fts_update BEFORE INSERT OR UPDATE ON public.items FOR EACH ROW EXECUTE FUNCTION public.items_fts_trigger();

ALTER TABLE ONLY public.items
    ADD CONSTRAINT items_library_id_fkey FOREIGN KEY (library_id, server_id) REFERENCES public.libraries(id, server_id) ON DELETE CASCADE;

ALTER TABLE ONLY public.items
    ADD CONSTRAINT items_parent_id_fkey FOREIGN KEY (parent_id, server_id) REFERENCES public.items(id, server_id) ON DELETE CASCADE;

ALTER TABLE ONLY public.libraries
    ADD CONSTRAINT libraries_server_id_fkey FOREIGN KEY (server_id) REFERENCES public.servers(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.media_parts
    ADD CONSTRAINT media_parts_item_id_fkey FOREIGN KEY (item_id, server_id) REFERENCES public.items(id, server_id) ON DELETE CASCADE;

ALTER TABLE ONLY public.streams
    ADD CONSTRAINT streams_media_part_id_fkey FOREIGN KEY (media_part_id, server_id) REFERENCES public.media_parts(id, server_id) ON DELETE CASCADE;

REVOKE USAGE ON SCHEMA public FROM PUBLIC;
GRANT ALL ON SCHEMA public TO PUBLIC;