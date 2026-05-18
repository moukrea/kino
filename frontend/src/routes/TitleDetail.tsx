// PRD §F-010 title detail view. Top-to-bottom:
//
//   - Backdrop image with bottom vignette
//   - Logo (if available) else stylized title
//   - Year • runtime • age rating • genres
//   - IMDb • TMDB • Trakt ratings (when known)
//   - Summary in user's primary language
//   - Cast row (top 6 with photos, TMDB credits)
//   - Action bar: Play / Resume + Mark Watched + Back
//   - Movies: stream list (PRD-locked sort)
//   - Series: season selector + episode list with progress bars +
//     per-episode stream list (fetched on demand)
//
// Back navigation pops the focus-restore stack so the originating tile
// regains focus (PRD §F-010 acceptance). This works for keyboard
// (Escape), gamepad (B button), and the on-screen Back button alike —
// they all funnel through `goBack`.

import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type Component,
} from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";

import { Focusable } from "../components/Focusable";
import {
  popReturnFocus,
  setFocusedId,
  setInitialFocus,
} from "../input/focus";
import { onAction } from "../input/keyboard";
import { locale } from "../i18n";
import { t } from "../i18n";
import {
  cwRecordPosition,
  getStreams,
  getTitleDetail,
  hasTauri,
  resolveArtwork,
  startPlayback,
  type Artwork,
  type Episode,
  type PlaybackSource,
  type PlayerCwContext,
  type StreamRow,
  type TitleDetail as TitleDetailData,
  type TitleKind,
} from "../lib/tauri";
import { setPlayerSession, type PlayerSessionState } from "../lib/playerSession";

/**
 * Convert a 0..10 rating into a fixed-decimal label or `null` for the
 * "absent" case (the Ratings row hides individual entries that are
 * absent per PRD §F-010 "only when known").
 */
function ratingLabel(n: number | null): string | null {
  if (n === null || Number.isNaN(n)) return null;
  return n.toFixed(1);
}

/**
 * Render a single piece of the metadata header "Year • runtime • age".
 * Returns `null` for absent fields so the caller filters them out
 * before joining.
 */
function metadataChips(data: TitleDetailData): string[] {
  const chips: string[] = [];
  if (data.year !== null) chips.push(String(data.year));
  if (data.runtime_minutes !== null) {
    chips.push(t("detail.minutes", { n: String(data.runtime_minutes) }));
  }
  if (data.age_rating !== null && data.age_rating.length > 0) {
    chips.push(data.age_rating);
  }
  return chips;
}

/**
 * Format `n` bytes as a human-readable size. Used by the stream row.
 * Returns the empty string when `n` is null so the caller can elide
 * the chip without an extra Show wrapper.
 */
function formatSize(n: number | null): string {
  if (n === null) return "";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  const trimmed = v >= 100 ? v.toFixed(0) : v.toFixed(1);
  return `${trimmed} ${units[i]}`;
}

/**
 * Default lang chain pulled from the active app locale. The full F-016
 * settings UI hasn't shipped yet, so we mirror the locale signal as the
 * single-entry lang chain. When F-016 lands, this becomes a
 * `kv_get("lang_pref")` read.
 */
function langChain(): string[] {
  return [locale()];
}

export const TitleDetailRoute: Component = () => {
  const params = useParams<{ id: string }>();
  const location = useLocation<{ from?: string }>();
  const navigate = useNavigate();

  const decodedId = createMemo(() => {
    try {
      return decodeURIComponent(params.id);
    } catch {
      return params.id;
    }
  });

  const kindParam = createMemo<TitleKind>(() => {
    const search = location.search;
    const matchSeries = /[?&]kind=series\b/.test(search);
    return matchSeries ? "series" : "movie";
  });

  // ---- detail metadata (single fetch) ----
  const detailKey = (): [string, TitleKind, string[]] => [
    decodedId(),
    kindParam(),
    langChain(),
  ];
  const [detailResource] = createResource<
    TitleDetailData | null,
    [string, TitleKind, string[]]
  >(detailKey, async ([id, kind, langs]): Promise<TitleDetailData | null> => {
    if (!hasTauri()) return null;
    try {
      return await getTitleDetail(id, kind, langs);
    } catch (e) {
      console.warn("get_title_detail failed", e);
      return null;
    }
  });

  // ---- artwork enrichment (parallel; prefers F-005 cascade output) ----
  const [artworkResource] = createResource<
    Artwork | null,
    [string, TitleKind, string[]]
  >(detailKey, async ([id, kind, langs]): Promise<Artwork | null> => {
    if (!hasTauri()) return null;
    try {
      return await resolveArtwork(id, kind, langs);
    } catch (e) {
      console.warn("resolve_artwork failed", e);
      return null;
    }
  });

  // ---- series episode selection ----
  // Selected season (1-based). Defaults to the resume_season when CW
  // points the user at an episode mid-series; falls back to season 1
  // otherwise. Selected episode mirrors the same logic — picking the
  // resume episode when available, else the first episode of the
  // selected season.
  const [selectedSeason, setSelectedSeason] = createSignal<number>(1);
  const [selectedEpisode, setSelectedEpisode] = createSignal<number>(1);

  // Auto-pick season/episode when the detail loads. Tracks the resource
  // so a late-arriving metadata fetch still claims initial selection.
  createEffect(() => {
    const data = detailResource();
    if (!data || data.kind !== "series" || data.episodes.length === 0) return;
    const seedSeason = data.resume_season ?? data.episodes[0]!.season;
    const seedEpisode = data.resume_episode ?? data.episodes[0]!.episode;
    setSelectedSeason(seedSeason);
    setSelectedEpisode(seedEpisode);
  });

  const seasonList = createMemo<number[]>(() => {
    const data = detailResource();
    if (!data || data.kind !== "series") return [];
    const set = new Set<number>();
    for (const ep of data.episodes) set.add(ep.season);
    return Array.from(set).sort((a, b) => a - b);
  });

  const episodesForSelectedSeason = createMemo<Episode[]>(() => {
    const data = detailResource();
    if (!data || data.kind !== "series") return [];
    return data.episodes.filter((e) => e.season === selectedSeason());
  });

  // ---- streams fetch (re-fires per (season, episode) for series) ----
  const streamsKey = (): [string, TitleKind, number | null, number | null] => [
    decodedId(),
    kindParam(),
    kindParam() === "series" ? selectedSeason() : null,
    kindParam() === "series" ? selectedEpisode() : null,
  ];
  const [streamsResource] = createResource<
    StreamRow[],
    [string, TitleKind, number | null, number | null]
  >(
    streamsKey,
    async ([id, kind, season, episode]): Promise<StreamRow[]> => {
      if (!hasTauri()) return [];
      try {
        return await getStreams(id, kind, season, episode);
      } catch (e) {
        console.warn("get_streams failed", e);
        return [];
      }
    },
  );

  // Artwork-vs-Cinemeta fallback memos. `Show`'s type inference gets
  // confused by chained `?.` accessors across two resources, so we read
  // each resource into a memo with the explicit `string | null`
  // contract first.
  const backdropUrl = createMemo<string | null>(() => {
    const art = artworkResource();
    if (art && art.backdrop) return art.backdrop;
    const detail = detailResource();
    if (detail && detail.backdrop) return detail.backdrop;
    return null;
  });
  const logoUrl = createMemo<string | null>(() => {
    const art = artworkResource();
    if (art && art.logo) return art.logo;
    const detail = detailResource();
    if (detail && detail.logo) return detail.logo;
    return null;
  });

  // ---- back navigation w/ focus restore ----

  const goBack = () => {
    const saved = popReturnFocus();
    navigate(-1);
    if (saved !== null) {
      // The originating route remounts on history.back; defer the focus
      // restore so the focusable has time to re-register.
      queueMicrotask(() => {
        setFocusedId(saved);
      });
    }
  };

  // Esc / gamepad B both fire the `back` action via the F-017 keymap.
  onMount(() => {
    const unsubscribe = onAction((action) => {
      if (action === "back") goBack();
    });
    onCleanup(unsubscribe);
  });

  // Initial focus on the Play / Resume button — the primary CTA.
  onMount(() => {
    queueMicrotask(() => {
      setInitialFocus("detail-play");
    });
  });

  // ---- Play / Resume / Mark Watched / stream-pick handlers ----

  /**
   * Convert a Stremio stream row to the {@link PlaybackSource}
   * discriminator [`startPlayback`] expects. Torrent rows are dispatched
   * via the engine using a synthesized magnet (PRD §F-013); raw HTTP
   * rows use the `directUrl` branch. Returns `null` when the row has
   * neither an info hash nor a URL — those rows are unplayable.
   */
  const streamToSource = (stream: StreamRow): PlaybackSource | null => {
    if (stream.info_hash) {
      const magnet = `magnet:?xt=urn:btih:${stream.info_hash}`;
      return {
        kind: "magnet",
        url: magnet,
        fileIndex: stream.file_idx,
      };
    }
    if (stream.url) {
      return {
        kind: "directUrl",
        url: stream.url,
        mime: null,
        fileName: null,
      };
    }
    return null;
  };

  /**
   * Spin up the embedded torrent engine (or pass through the direct URL)
   * for the selected stream, then navigate to `/player` with the session
   * state. The Player route boots the F-015 driver from there.
   */
  const launchStream = async (stream: StreamRow): Promise<void> => {
    const data: TitleDetailData | null | undefined = detailResource();
    if (!data || data.stremio_id === null) return;
    if (!hasTauri()) return;
    const source = streamToSource(stream);
    if (!source) {
      console.warn("stream has neither url nor info_hash; skipping");
      return;
    }
    try {
      const handle = await startPlayback(source);
      const season = data.kind === "series" ? selectedSeason() : 0;
      const episode = data.kind === "series" ? selectedEpisode() : 0;
      const episodes: [number, number][] =
        data.kind === "series"
          ? data.episodes.map((ep) => [ep.season, ep.episode] as [number, number])
          : [];
      const meta = {
        title: data.title,
        year: data.year,
        poster: data.poster,
        rating: data.imdb_rating,
      };
      const cwContext: PlayerCwContext = {
        titleId: data.stremio_id,
        kind: data.kind,
        season,
        episode,
        metaJson: meta,
        episodes,
      };
      const resumePositionS =
        data.resume_position_s !== null &&
        data.resume_season === season &&
        data.resume_episode === episode
          ? data.resume_position_s
          : 0;
      const durationHintS =
        data.runtime_minutes !== null ? data.runtime_minutes * 60 : null;
      const sessionState: PlayerSessionState = {
        token: handle.token,
        url: handle.url,
        resumePositionS,
        fileName: handle.fileName,
        durationHintS,
        cwContext,
        displayTitle: data.title,
      };
      setPlayerSession(sessionState);
      navigate("/player");
    } catch (e) {
      console.warn("startPlayback failed", e);
    }
  };

  /**
   * Play / Resume action-bar handler. Picks the first stream in the
   * sort-locked list (PRD §F-010: `quality DESC, seeders DESC, size DESC`)
   * and dispatches it via [`launchStream`]. When no streams are loaded
   * yet the button is a no-op — the user can still pick from the list
   * directly. Resume position is derived inside [`launchStream`] from
   * the CW row regardless of which button the user pressed, so there's
   * no per-button branch here.
   */
  const playOrResume = (): void => {
    const streams = streamsResource();
    if (!streams || streams.length === 0) {
      return;
    }
    void launchStream(streams[0]!);
  };

  const markWatched = () => {
    const data: TitleDetailData | null | undefined = detailResource();
    if (!data || !hasTauri() || data.stremio_id === null) return;
    const season =
      data.kind === "series" ? selectedSeason() : 0;
    const episode =
      data.kind === "series" ? selectedEpisode() : 0;
    const duration =
      data.runtime_minutes !== null ? data.runtime_minutes * 60 : 1;
    // Mark watched routes through the F-012 canonical writer so the
    // locked rules apply: movies stay at duration (the 24h sweep
    // expires them), series at the last episode trigger the
    // remove-series rule, series mid-list advance to the next episode.
    const episodes: ReadonlyArray<readonly [number, number]> =
      data.kind === "series"
        ? data.episodes.map((ep) => [ep.season, ep.episode] as const)
        : [];
    cwRecordPosition(
      {
        title_id: data.stremio_id,
        kind: data.kind,
        season,
        episode,
        position_s: duration,
        duration_s: duration,
        last_played_at: Math.floor(Date.now() / 1000),
        meta_json: {
          title: data.title,
          year: data.year,
          poster: data.poster,
          rating: data.imdb_rating,
        },
      },
      episodes,
    ).catch((e) => console.warn("cw_record_position failed", e));
  };

  return (
    <div
      class="relative flex h-full w-full flex-col overflow-y-auto bg-neutral-950 text-neutral-50"
      data-testid="title-detail-root"
    >
      {/* Backdrop with bottom vignette per PRD §F-010. */}
      <Show when={backdropUrl()}>
        {(url) => (
          <div
            class="pointer-events-none absolute inset-x-0 top-0 h-[55vh] w-full overflow-hidden"
            data-testid="detail-backdrop"
          >
            <img
              src={url()}
              alt=""
              loading="eager"
              decoding="async"
              class="h-full w-full object-cover opacity-60"
            />
            <div class="absolute inset-0 bg-gradient-to-t from-neutral-950 via-neutral-950/50 to-transparent" />
          </div>
        )}
      </Show>

      <div class="relative z-10 flex flex-col gap-6 px-8 pb-12 pt-6">
        <Focusable id="detail-back" onActivate={goBack}>
          {({ ref, showRing, onClick }) => (
            <button
              ref={ref as (el: HTMLButtonElement) => void}
              type="button"
              onClick={onClick}
              data-testid="detail-back-button"
              class={`self-start rounded bg-neutral-800/80 px-3 py-1 text-sm transition-transform ${
                showRing()
                  ? "scale-[1.05] outline outline-2 outline-sky-400"
                  : ""
              }`}
            >
              ← {t("detail.back")}
            </button>
          )}
        </Focusable>

        <Show
          when={detailResource()}
          keyed
          fallback={
            <div class="pt-32 text-center text-lg" data-testid="detail-loading">
              {t("detail.loading")}
            </div>
          }
        >
          {(detail: TitleDetailData) => (
            <div class="flex flex-col gap-6 pt-24">
                {/* Logo or stylized title */}
                <Show
                  when={logoUrl()}
                  fallback={
                    <h1
                      class="text-5xl font-black tracking-tight"
                      data-testid="detail-title-text"
                    >
                      {detail.title}
                    </h1>
                  }
                >
                  {(logoUrl) => (
                    <img
                      src={logoUrl()}
                      alt={detail.title}
                      data-testid="detail-logo"
                      class="max-h-32 self-start object-contain"
                    />
                  )}
                </Show>

                {/* Year • runtime • age • genres */}
                <div
                  class="flex flex-wrap items-center gap-x-3 text-sm text-neutral-300"
                  data-testid="detail-metadata-chips"
                >
                  <For each={metadataChips(detail)}>
                    {(chip) => <span>{chip}</span>}
                  </For>
                  <For each={detail.genres}>
                    {(g) => (
                      <span class="rounded border border-neutral-700 px-2 py-0.5 text-xs uppercase tracking-wide">
                        {g}
                      </span>
                    )}
                  </For>
                </div>

                {/* Ratings (only when known) */}
                <Show when={hasAnyRating(detail)}>
                  <div
                    class="flex flex-wrap gap-4 text-sm"
                    data-testid="detail-ratings"
                  >
                    <Show when={ratingLabel(detail.imdb_rating)}>
                      {(v) => (
                        <span data-testid="detail-rating-imdb">
                          {t("detail.ratingImdb")} ★ {v()}
                        </span>
                      )}
                    </Show>
                    <Show when={ratingLabel(detail.tmdb_rating)}>
                      {(v) => (
                        <span data-testid="detail-rating-tmdb">
                          {t("detail.ratingTmdb")} ★ {v()}
                        </span>
                      )}
                    </Show>
                    <Show when={ratingLabel(detail.trakt_rating)}>
                      {(v) => (
                        <span data-testid="detail-rating-trakt">
                          {t("detail.ratingTrakt")} ★ {v()}
                        </span>
                      )}
                    </Show>
                  </div>
                </Show>

                {/* Summary */}
                <Show when={detail.summary}>
                  {(summary) => (
                    <p
                      class="max-w-3xl text-base leading-relaxed text-neutral-100"
                      data-testid="detail-summary"
                    >
                      {summary()}
                    </p>
                  )}
                </Show>

                {/* Cast row */}
                <Show when={detail.cast.length > 0}>
                  <section
                    class="flex flex-col gap-3"
                    data-testid="detail-cast-row"
                  >
                    <h2 class="text-lg font-semibold">{t("detail.cast")}</h2>
                    <div class="flex gap-4 overflow-x-auto pb-2">
                      <For each={detail.cast}>
                        {(member) => (
                          <div class="flex w-28 flex-shrink-0 flex-col items-center gap-1 text-center">
                            <div class="h-28 w-28 overflow-hidden rounded-full bg-neutral-800">
                              <Show
                                when={member.photo}
                                fallback={
                                  <div class="flex h-full w-full items-center justify-center text-xs text-neutral-500">
                                    {member.name
                                      .split(" ")
                                      .map((p) => p[0]?.toUpperCase() ?? "")
                                      .join("")
                                      .slice(0, 2)}
                                  </div>
                                }
                              >
                                {(photo) => (
                                  <img
                                    src={photo()}
                                    alt={member.name}
                                    loading="lazy"
                                    decoding="async"
                                    class="h-full w-full object-cover"
                                  />
                                )}
                              </Show>
                            </div>
                            <div class="text-xs font-medium leading-tight text-neutral-100">
                              {member.name}
                            </div>
                            <Show when={member.character}>
                              {(c) => (
                                <div class="text-[10px] leading-tight text-neutral-400">
                                  {c()}
                                </div>
                              )}
                            </Show>
                          </div>
                        )}
                      </For>
                    </div>
                  </section>
                </Show>

                {/* Action bar */}
                <div
                  class="flex flex-wrap gap-3"
                  data-testid="detail-action-bar"
                >
                  <Focusable id="detail-play" onActivate={playOrResume}>
                    {({ ref, showRing, onClick }) => (
                      <button
                        ref={ref as (el: HTMLButtonElement) => void}
                        type="button"
                        onClick={onClick}
                        data-testid={
                          detail.resume_position_s !== null
                            ? "detail-resume-button"
                            : "detail-play-button"
                        }
                        class={`rounded bg-sky-500 px-6 py-2 text-base font-semibold text-neutral-950 transition-transform ${
                          showRing()
                            ? "scale-[1.05] outline outline-2 outline-sky-200"
                            : ""
                        }`}
                      >
                        {detail.resume_position_s !== null
                          ? t("detail.resume")
                          : t("detail.play")}
                      </button>
                    )}
                  </Focusable>

                  <Focusable id="detail-mark-watched" onActivate={markWatched}>
                    {({ ref, showRing, onClick }) => (
                      <button
                        ref={ref as (el: HTMLButtonElement) => void}
                        type="button"
                        onClick={onClick}
                        data-testid="detail-mark-watched-button"
                        class={`rounded border border-neutral-600 px-4 py-2 text-sm font-medium transition-transform ${
                          showRing()
                            ? "scale-[1.05] outline outline-2 outline-sky-400"
                            : ""
                        }`}
                      >
                        {t("detail.markWatched")}
                      </button>
                    )}
                  </Focusable>
                </div>

                {/* Series episodes (with progress bars) */}
                <Show when={detail.kind === "series" && detail.episodes.length > 0}>
                  <section
                    class="flex flex-col gap-3"
                    data-testid="detail-episodes-section"
                  >
                    <div class="flex items-center gap-3">
                      <h2 class="text-lg font-semibold">
                        {t("detail.seasons")}
                      </h2>
                      <div class="flex gap-2">
                        <For each={seasonList()}>
                          {(season) => (
                            <Focusable
                              id={`detail-season-${season}`}
                              onActivate={() => {
                                setSelectedSeason(season);
                                const eps = detail.episodes.filter(
                                  (e) => e.season === season,
                                );
                                if (eps.length > 0) {
                                  setSelectedEpisode(eps[0]!.episode);
                                }
                              }}
                            >
                              {({ ref, showRing, onClick }) => (
                                <button
                                  ref={ref as (el: HTMLButtonElement) => void}
                                  type="button"
                                  onClick={onClick}
                                  data-testid={`detail-season-button-${season}`}
                                  data-selected={
                                    season === selectedSeason() ? "true" : "false"
                                  }
                                  class={`rounded px-3 py-1 text-sm font-medium transition-transform ${
                                    season === selectedSeason()
                                      ? "bg-sky-500 text-neutral-950"
                                      : "border border-neutral-700 text-neutral-200"
                                  } ${
                                    showRing()
                                      ? "outline outline-2 outline-sky-300"
                                      : ""
                                  }`}
                                >
                                  {t("detail.seasonLabel", {
                                    n: String(season),
                                  })}
                                </button>
                              )}
                            </Focusable>
                          )}
                        </For>
                      </div>
                    </div>
                    <h3
                      class="text-base font-semibold text-neutral-200"
                      data-testid="detail-episodes-heading"
                    >
                      {t("detail.episodes")}
                    </h3>
                    <ul
                      class="flex flex-col gap-2"
                      data-testid="detail-episode-list"
                    >
                      <For each={episodesForSelectedSeason()}>
                        {(ep) => (
                          <li
                            class="flex items-stretch gap-3 rounded border border-neutral-800 bg-neutral-900/60 p-2"
                            data-testid={`detail-episode-${ep.season}-${ep.episode}`}
                            data-season={ep.season}
                            data-episode={ep.episode}
                          >
                            <Focusable
                              id={`detail-episode-${ep.season}-${ep.episode}`}
                              onActivate={() => {
                                setSelectedSeason(ep.season);
                                setSelectedEpisode(ep.episode);
                              }}
                            >
                              {({ ref, showRing, onClick }) => (
                                <button
                                  ref={ref as (el: HTMLButtonElement) => void}
                                  type="button"
                                  onClick={onClick}
                                  class={`flex w-full items-stretch gap-3 text-left transition-transform ${
                                    showRing()
                                      ? "outline outline-2 outline-sky-400"
                                      : ""
                                  }`}
                                >
                                  <Show
                                    when={ep.thumbnail}
                                    fallback={
                                      <div class="h-20 w-32 flex-shrink-0 rounded bg-neutral-800" />
                                    }
                                  >
                                    {(thumb) => (
                                      <img
                                        src={thumb()}
                                        alt=""
                                        loading="lazy"
                                        decoding="async"
                                        class="h-20 w-32 flex-shrink-0 rounded object-cover"
                                      />
                                    )}
                                  </Show>
                                  <div class="flex flex-1 flex-col gap-1">
                                    <div class="flex items-baseline justify-between gap-2">
                                      <div class="text-sm font-semibold">
                                        {t("detail.episodeLabel", {
                                          season: String(ep.season),
                                          episode: String(ep.episode),
                                        })}{" "}
                                        — {ep.title}
                                      </div>
                                      <Show when={ep.air_date}>
                                        {(d) => (
                                          <div class="text-xs text-neutral-400">
                                            {d()}
                                          </div>
                                        )}
                                      </Show>
                                    </div>
                                    <Show when={ep.overview}>
                                      {(o) => (
                                        <div class="text-xs text-neutral-300">
                                          {o()}
                                        </div>
                                      )}
                                    </Show>
                                    {/* Per-episode progress bar (PRD F-010). */}
                                    <div
                                      class="mt-1 h-1 w-full overflow-hidden rounded bg-neutral-700"
                                      data-testid={`detail-episode-progress-${ep.season}-${ep.episode}`}
                                      data-progress={ep.progress.toFixed(3)}
                                    >
                                      <div
                                        class="h-full bg-sky-500"
                                        style={{
                                          width: `${Math.max(0, Math.min(1, ep.progress)) * 100}%`,
                                        }}
                                      />
                                    </div>
                                  </div>
                                </button>
                              )}
                            </Focusable>
                          </li>
                        )}
                      </For>
                    </ul>
                  </section>
                </Show>

                {/* Streams */}
                <section
                  class="flex flex-col gap-2"
                  data-testid="detail-streams-section"
                >
                  <h2 class="text-lg font-semibold">{t("detail.streams")}</h2>
                  <Show
                    when={streamsResource()}
                    keyed
                    fallback={
                      <div
                        class="text-sm text-neutral-500"
                        data-testid="detail-streams-loading"
                      >
                        {t("detail.loadingStreams")}
                      </div>
                    }
                  >
                    {(streams: StreamRow[]) => (
                      <Show
                        when={streams.length > 0}
                        fallback={
                          <div
                            class="text-sm text-neutral-500"
                            data-testid="detail-streams-empty"
                          >
                            {t("detail.noStreams")}
                          </div>
                        }
                      >
                        <ul class="flex flex-col gap-1">
                          <For each={streams}>
                            {(s, idx) => (
                              <Focusable
                                id={`detail-stream-${idx()}`}
                                onActivate={() => void launchStream(s)}
                              >
                                {({ ref, showRing, onClick }) => (
                                  <li
                                    class="list-none"
                                    data-testid={`detail-stream-${idx()}`}
                                  >
                                    <button
                                      ref={ref as (el: HTMLButtonElement) => void}
                                      type="button"
                                      onClick={onClick}
                                      class={`flex w-full flex-col gap-1 rounded border border-neutral-800 bg-neutral-900/60 p-3 text-left transition-transform ${
                                        showRing()
                                          ? "scale-[1.01] outline outline-2 outline-sky-400"
                                          : ""
                                      }`}
                                    >
                                      <div class="flex flex-wrap items-center gap-2 text-xs">
                                        <Show when={s.quality}>
                                          {(q) => (
                                            <span
                                              class="rounded bg-sky-500 px-2 py-0.5 font-semibold text-neutral-950"
                                              data-testid="badge-quality"
                                            >
                                              {q()}
                                            </span>
                                          )}
                                        </Show>
                                        <Show when={s.hdr}>
                                          {(h) => (
                                            <span
                                              class="rounded bg-amber-500 px-2 py-0.5 font-semibold text-neutral-950"
                                              data-testid="badge-hdr"
                                            >
                                              {h()}
                                            </span>
                                          )}
                                        </Show>
                                        <Show when={s.audio}>
                                          {(a) => (
                                            <span
                                              class="rounded border border-neutral-600 px-2 py-0.5 font-semibold"
                                              data-testid="badge-audio"
                                            >
                                              {a()}
                                            </span>
                                          )}
                                        </Show>
                                        <Show when={s.codec}>
                                          {(c) => (
                                            <span
                                              class="rounded border border-neutral-600 px-2 py-0.5 font-semibold"
                                              data-testid="badge-codec"
                                            >
                                              {c()}
                                            </span>
                                          )}
                                        </Show>
                                        <span class="text-neutral-400">
                                          {s.addon_name}
                                        </span>
                                        <Show when={s.seeders !== null}>
                                          <span
                                            class="text-neutral-400"
                                            data-testid="badge-seeders"
                                          >
                                            ▲ {s.seeders}
                                          </span>
                                        </Show>
                                        <Show when={s.size_bytes !== null}>
                                          <span
                                            class="text-neutral-400"
                                            data-testid="badge-size"
                                          >
                                            {formatSize(s.size_bytes)}
                                          </span>
                                        </Show>
                                      </div>
                                      <div class="truncate text-xs text-neutral-300">
                                        {s.detail ?? s.name}
                                      </div>
                                    </button>
                                  </li>
                                )}
                              </Focusable>
                            )}
                          </For>
                        </ul>
                      </Show>
                    )}
                  </Show>
                </section>
              </div>
          )}
        </Show>

        <Show when={detailResource.error}>
          <div class="pt-32 text-center text-red-400" data-testid="detail-error">
            {t("detail.error")}
          </div>
        </Show>
      </div>
    </div>
  );
};

function hasAnyRating(data: TitleDetailData): boolean {
  return (
    data.imdb_rating !== null ||
    data.tmdb_rating !== null ||
    data.trakt_rating !== null
  );
}

// Keep the legacy named export so App.tsx doesn't need a separate
// import line; both names refer to the same component.
export { TitleDetailRoute as TitleDetail };

export default TitleDetailRoute;
