//! Home page: live FM station dashboard + voting UI.
//!
//! Visually this is a warm mid-century console radio (the "rtl-fm vote"
//! design handoff): a dark wood/metal case, a glowing horizontal tuning
//! dial as the hero, an analog VU signal meter, an amp-style rotary
//! quality knob, a live waveform scope, and a cream "station log"
//! ledger. The behaviour underneath is unchanged from the original
//! debug dashboard:
//!
//! The page subscribes to `/api/stations/stream` for a 1 Hz feed of
//! every scanned station, its current RDS/HD metadata, its vote tally,
//! and the currently-winning frequency.
//!
//! Each tab computes a SHA-256 "fingerprint" from canvas + WebGL +
//! audio-context quirks + UA + screen + timezone, cached in
//! `localStorage`. That keeps the same Chrome profile collapsed to a
//! single vote across tabs (shared `localStorage` + identical render
//! quirks) while different profiles on the same machine each get
//! their own vote.
//!
//! When the user clicks Vote on a row the page starts heartbeating
//! `POST /api/vote { fingerprint, freq_hz }` every 4 s. The heartbeat
//! keeps running while the tab is hidden so browser tab switches do
//! not expire the active stream. The backend votes expire 12 s after
//! the last heartbeat. On unload we best-effort `POST /api/unvote` so
//! the voter count drops promptly when the window closes.
//!
//! A single `<audio>` element points at `/stream.aac`. Whenever the
//! winner changes the server-side vote daemon aborts the channel,
//! the audio element fires `error`, and the script reopens the URL —
//! at which point the new winner is in effect. The custom transport
//! (play/pause + volume) drives that hidden element directly.

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::{Html, IntoResponse};

use crate::api::auth::{self, ADMIN_COOKIE, ANON_COOKIE};
use crate::api::AppState;

/// Serves the page and manages the auth/session cookies:
/// * a valid `?token=` mints the `rtlfm_admin` cookie,
/// * every visitor without one gets a random `rtlfm_anon` cookie so the
///   server can account for anonymous public users.
pub async fn index(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    let mut out = HeaderMap::new();

    if let Some(tok) = q.get("token") {
        if auth::ct_eq(tok, &state.admin_token) {
            let c = format!(
                "{ADMIN_COOKIE}={tok}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000"
            );
            if let Ok(v) = HeaderValue::from_str(&c) {
                out.append(header::SET_COOKIE, v);
            }
        }
    }

    let cookies = auth::parse_cookies(&headers);
    match cookies.get(ANON_COOKIE) {
        Some(id) => state.sessions.touch(id),
        None => {
            let id = auth::gen_token();
            state.sessions.touch(&id);
            let c =
                format!("{ANON_COOKIE}={id}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000");
            if let Ok(v) = HeaderValue::from_str(&c) {
                out.append(header::SET_COOKIE, v);
            }
        }
    }

    (out, Html(PAGE))
}

const PAGE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>RTL FM</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Bitter:ital,wght@0,400;0,500;0,600;0,700;0,800;1,500&family=Barlow+Semi+Condensed:wght@400;500;600;700&family=DM+Mono:wght@400;500&display=swap" rel="stylesheet">
<style>
  :root {
    --case:      #241c14;
    --case-hi:   #34281b;
    --case-lo:   #15100b;
    --case-edge: #0c0907;
    --grille:    #1c150e;

    --cream:     #f1e6c8;
    --cream-dim: #cdbf9c;
    --glow:      #f6e6ad;
    --ink:       #2a2015;
    --ink-dim:   #7a6a4f;
    --ink-faint: #a8997a;

    --amber:        #e6a23a;
    --amber-bright: #f6c25a;
    --needle:       #d8472c;
    --onair:        #ef5230;
    --vote-lamp:    #5fb87a;
    --backlight:    rgba(246,194,90,.55);

    --rad: 14px;
    --shadow-deep: 0 30px 80px -20px rgba(0,0,0,.7);
  }

  * { box-sizing: border-box; }
  html, body { margin: 0; padding: 0; }
  body {
    font-family: "Barlow Semi Condensed", system-ui, sans-serif;
    background:
      radial-gradient(120% 90% at 50% -10%, #2a211a 0%, #181208 55%, #0c0805 100%);
    background-attachment: fixed;
    color: var(--cream);
    min-height: 100vh;
    -webkit-font-smoothing: antialiased;
    padding: 36px 20px 60px;
  }
  .mono  { font-family: "DM Mono", ui-monospace, monospace; }
  .serif { font-family: "Bitter", Georgia, serif; }

  #root { max-width: 1180px; margin: 0 auto; }

  /* ============ CASE ============ */
  .case {
    position: relative;
    border-radius: 22px;
    padding: 26px;
    background:
      linear-gradient(180deg, var(--case-hi), var(--case) 28%, var(--case-lo) 100%);
    box-shadow:
      var(--shadow-deep),
      inset 0 2px 0 rgba(255,225,170,.10),
      inset 0 -3px 8px rgba(0,0,0,.55),
      inset 0 0 0 2px rgba(0,0,0,.35);
  }
  .case::before {
    content: "";
    position: absolute; inset: 0;
    border-radius: 22px;
    pointer-events: none;
    background:
      repeating-linear-gradient(92deg, rgba(255,255,255,.018) 0 2px, rgba(0,0,0,.02) 2px 5px);
    mix-blend-mode: overlay;
    opacity: .6;
  }
  .screw {
    position: absolute; width: 13px; height: 13px; border-radius: 50%;
    background: radial-gradient(circle at 35% 30%, #6a5a44, #2a2014 70%);
    box-shadow: inset 0 1px 1px rgba(255,235,200,.35), 0 1px 2px rgba(0,0,0,.6);
  }
  .screw::after {
    content: ""; position: absolute; top: 50%; left: 50%;
    width: 8px; height: 1.5px; border-radius: 1px;
    background: rgba(0,0,0,.55);
    box-shadow: 0 1px 0 rgba(255,235,200,.18);
    transform: translate(-50%, -50%) rotate(35deg);
  }
  .screw.tl { top: 12px; left: 12px; }
  .screw.tr { top: 12px; right: 12px; }
  .screw.bl { bottom: 12px; left: 12px; }
  .screw.br { bottom: 12px; right: 12px; }

  /* ============ TOP PLATE ============ */
  .plate {
    display: flex; align-items: flex-end; justify-content: space-between;
    gap: 20px; padding: 2px 10px 18px; flex-wrap: wrap;
  }
  .brand { display: flex; flex-direction: column; align-items: flex-start; gap: 3px; flex-shrink: 0; }
  .brand h1 {
    font-family: "Bitter", serif; font-weight: 800; font-size: 34px;
    margin: 0; letter-spacing: .5px; color: var(--cream);
    text-shadow: 0 1px 0 rgba(0,0,0,.6), 0 0 22px rgba(246,194,90,.18);
  }
  .brand-sub {
    font-size: 12px; letter-spacing: .14em; text-transform: uppercase;
    color: var(--ink-faint);
  }
  .brand-sub:empty { display: none; }
  .lbl {
    font-size: 11px; letter-spacing: .2em; text-transform: uppercase;
    color: var(--ink-faint); font-weight: 600;
  }

  .btn {
    font-family: "Barlow Semi Condensed", sans-serif;
    font-weight: 600; letter-spacing: .12em; text-transform: uppercase;
    font-size: 12.5px; color: #f3ead2;
    background: linear-gradient(180deg, #4a3a28, #2c2114);
    border: 1px solid rgba(0,0,0,.5);
    border-radius: 7px; padding: 9px 14px; cursor: pointer; white-space: nowrap;
    box-shadow: inset 0 1px 0 rgba(255,225,170,.22), 0 2px 4px rgba(0,0,0,.4);
    transition: transform .08s, filter .15s;
  }
  .btn:hover { filter: brightness(1.12); }
  .btn:active { transform: translateY(1px); }
  .btn:disabled { cursor: default; opacity: .7; }

  /* Rescan is an admin-only control; revealed once /api/whoami confirms the
     admin token. The server enforces it regardless of this. */
  #rescan { display: none; }
  body.is-admin #rescan { display: inline-block; }

  /* range inputs (volume) */
  input[type=range].slab {
    -webkit-appearance: none; appearance: none;
    width: 100%; height: 26px; background: transparent; cursor: pointer;
  }
  input[type=range].slab::-webkit-slider-runnable-track {
    height: 6px; border-radius: 4px;
    background: linear-gradient(90deg, var(--amber) var(--fill,40%), #1a130b var(--fill,40%));
    box-shadow: inset 0 1px 2px rgba(0,0,0,.7);
  }
  input[type=range].slab::-moz-range-track {
    height: 6px; border-radius: 4px; background: #1a130b;
    box-shadow: inset 0 1px 2px rgba(0,0,0,.7);
  }
  input[type=range].slab::-webkit-slider-thumb {
    -webkit-appearance: none; appearance: none;
    width: 16px; height: 22px; margin-top: -8px; border-radius: 4px;
    background: linear-gradient(180deg, #f0e6cc, #b9a578);
    border: 1px solid #2a2014;
    box-shadow: 0 2px 4px rgba(0,0,0,.6), inset 0 1px 0 #fff7e3;
  }
  input[type=range].slab::-moz-range-thumb {
    width: 16px; height: 22px; border-radius: 4px; border: 1px solid #2a2014;
    background: linear-gradient(180deg, #f0e6cc, #b9a578);
    box-shadow: 0 2px 4px rgba(0,0,0,.6);
  }

  /* ============ DIAL ============ */
  .dial-shell {
    padding: 12px; border-radius: 12px;
    background: linear-gradient(180deg,#0c0907,#1a130c);
    box-shadow: inset 0 2px 8px rgba(0,0,0,.7), 0 1px 0 rgba(255,225,170,.08);
  }
  .dial-glass {
    position: relative; height: 150px; border-radius: 8px; overflow: hidden;
    background: linear-gradient(180deg, #f6ecca 0%, #efe1ba 42%, #e6d3a3 100%);
    box-shadow:
      inset 0 0 0 2px rgba(120,95,55,.4),
      inset 0 14px 30px rgba(255,247,210,.6),
      inset 0 -10px 24px rgba(150,115,60,.35);
    transition: filter .2s;
  }
  .dial-glass.scanning { filter: brightness(1.15); }
  .dial-backlight {
    position: absolute; inset: 0; pointer-events: none; transition: opacity .3s;
    background: radial-gradient(90% 130% at 50% 120%, var(--backlight) 0%, rgba(246,194,90,0) 60%);
  }
  .dial-band {
    position: absolute; top: 12px; left: 14px;
    font-family: "Bitter", serif; font-weight: 700; font-size: 15px;
    letter-spacing: .22em; color: rgba(60,45,25,.7);
  }
  .dial-mhz {
    position: absolute; top: 14px; right: 16px;
    font-size: 11px; letter-spacing: .24em; color: rgba(60,45,25,.55);
    text-transform: uppercase; font-weight: 600;
  }
  .dial-tick { position: absolute; top: 40px; width: 2px; transform: translateX(-50%); background: rgba(58,42,22,.85); }
  .dial-num {
    position: absolute; top: 60px; transform: translateX(-50%);
    font-family: "Bitter", serif; font-weight: 700; font-size: 22px; color: #3a2a16;
  }
  .dial-lamp {
    position: absolute; top: 98px; width: 11px; height: 11px; border-radius: 50%;
    border: 1px solid rgba(60,45,25,.4); padding: 0; cursor: pointer;
    transition: transform .3s, box-shadow .3s, background .3s;
  }
  .dial-needle { position: absolute; top: 0; bottom: 0; width: 0; z-index: 5; pointer-events: none; }
  .dial-needle-head {
    position: absolute; top: 30px; left: 50%; transform: translateX(-50%);
    width: 0; height: 0; border-left: 7px solid transparent; border-right: 7px solid transparent;
    border-top: 11px solid var(--needle); filter: drop-shadow(0 0 4px rgba(216,71,44,.7));
  }
  .dial-needle-line {
    position: absolute; top: 40px; left: 50%; transform: translateX(-50%);
    width: 2.5px; height: 92px;
    background: linear-gradient(180deg, var(--needle), #8e2c1a);
    box-shadow: 0 0 8px rgba(216,71,44,.8);
  }

  /* ============ PLAYER ============ */
  .pl-wrap {
    display: flex; flex-direction: column; gap: 18px; margin-top: 16px;
    background: linear-gradient(180deg,#2a2014,#1b150d);
    border: 1px solid rgba(0,0,0,.45); border-radius: 12px; padding: 20px 22px;
    box-shadow: inset 0 1px 0 rgba(255,225,170,.1), 0 6px 18px rgba(0,0,0,.35);
  }
  .pl-toprow { display: flex; align-items: flex-start; justify-content: space-between; gap: 24px; }
  .pl-readout { position: relative; min-width: 0; flex: 1; }
  .pl-headline { display: flex; align-items: baseline; gap: 12px; flex-wrap: wrap; }
  .pl-station {
    font-weight: 800; font-size: 30px; color: var(--cream); line-height: 1;
    letter-spacing: .5px; white-space: nowrap;
  }
  .pl-freqtag {
    display: inline-flex; align-items: baseline; gap: 4px;
    font-size: 20px; font-weight: 500; color: var(--amber-bright);
    letter-spacing: .01em; text-shadow: 0 0 14px rgba(246,194,90,.35);
  }
  .pl-frequnit { font-size: 11px; color: var(--ink-faint); letter-spacing: .12em; text-transform: uppercase; }
  .pl-call { font-size: 12px; color: var(--ink-faint); letter-spacing: .06em; margin-top: 5px; }
  .pl-np {
    margin-top: 7px; font-size: 16px; color: var(--cream-dim); max-width: 620px; height: 22px;
    overflow: hidden; white-space: nowrap; text-overflow: ellipsis;
  }
  .pl-nptext { font-style: italic; }

  .pl-controlbar {
    display: flex; align-items: center; gap: 14px;
    padding-top: 16px; border-top: 1px solid rgba(255,225,170,.08);
  }
  .pl-playbtn {
    width: 44px; height: 44px; border-radius: 50%; flex-shrink: 0;
    background: radial-gradient(circle at 35% 30%, #6a5236, #2c2114);
    border: 1px solid rgba(0,0,0,.5);
    box-shadow: inset 0 1px 0 rgba(255,225,170,.3), 0 2px 5px rgba(0,0,0,.5);
    display: flex; align-items: center; justify-content: center; cursor: pointer; padding: 0;
  }
  .pl-playglyph {
    width: 0; height: 0; margin-left: 3px;
    border-top: 9px solid transparent; border-bottom: 9px solid transparent;
    border-left: 14px solid var(--amber-bright);
  }
  .pl-pauseglyph { display: flex; gap: 4px; }
  .pl-pausebar { width: 4px; height: 16px; background: var(--amber-bright); border-radius: 1px; }
  .pl-scope {
    flex: 1; height: 44px; min-width: 80px; border-radius: 6px; overflow: hidden;
    background: linear-gradient(180deg,#0c0907,#15100b);
    box-shadow: inset 0 1px 4px rgba(0,0,0,.8), inset 0 0 0 1px rgba(255,225,170,.06);
  }
  .pl-divider { width: 1px; height: 30px; background: rgba(255,225,170,.12); flex-shrink: 0; }
  .pl-volrow { display: flex; align-items: center; gap: 9px; flex-shrink: 0; }

  /* instrument boxes (VU meter + quality knob) */
  .inst-shell { width: 196px; flex-shrink: 0; }
  .inst-face {
    background: linear-gradient(180deg,#f2e7c8,#e2cf9f);
    border-radius: 8px 8px 6px 6px; height: 108px;
    display: flex; align-items: center; justify-content: center;
    box-shadow: inset 0 0 0 2px rgba(120,95,55,.35), inset 0 6px 14px rgba(255,247,210,.6);
  }
  .inst-cap { display: flex; justify-content: space-between; align-items: center; gap: 10px; padding: 6px 4px 0; }
  .inst-cap .mono { color: var(--amber-bright); font-size: 12px; white-space: nowrap; }
  .knob-hit { cursor: grab; user-select: none; -webkit-user-select: none; touch-action: none; flex-shrink: 0; }
  .vu-needle { transition: transform .85s cubic-bezier(.34,1.36,.5,1); }
  .q-pointer { transition: transform .3s cubic-bezier(.34,1.4,.5,1); }

  /* ============ LEDGER ============ */
  .lg-panel {
    margin-top: 18px;
    background: linear-gradient(180deg,#f4ead0,#ecdfbf);
    border-radius: 12px;
    box-shadow: inset 0 0 0 2px rgba(120,95,55,.3), 0 8px 22px rgba(0,0,0,.4);
    overflow: hidden;
  }
  .lg-head {
    display: flex; align-items: center; justify-content: space-between; gap: 16px;
    padding: 12px 18px; border-bottom: 1px solid rgba(120,95,55,.25);
    background: linear-gradient(180deg, rgba(255,247,220,.6), rgba(0,0,0,0));
  }
  .lg-hint { font-size: 13px; color: var(--ink-dim); font-style: italic; }
  .lg-scroll { max-height: 440px; overflow-y: auto; }
  .lg-table { width: 100%; border-collapse: collapse; font-family: "Barlow Semi Condensed", sans-serif; }
  .lg-th {
    position: sticky; top: 0; z-index: 2; background: #e7d8b3;
    font-size: 11px; letter-spacing: .14em; text-transform: uppercase;
    color: var(--ink-dim); font-weight: 700; padding: 9px 10px; text-align: left;
    border-bottom: 1px solid rgba(120,95,55,.35);
  }
  .lg-th.center { text-align: center; }
  .lg-tr { border-bottom: 1px solid rgba(120,95,55,.15); transition: background .15s; }
  .lg-tr.on { background: rgba(239,82,48,.10); }
  .lg-tr.refreshing { background: rgba(230,162,58,.12); }
  .lg-td { padding: 9px 10px; vertical-align: middle; color: var(--ink); }
  .lg-tdfreq { padding: 9px 12px 9px 16px; position: relative; white-space: nowrap; }
  .lg-freqnum { font-weight: 700; font-size: 16px; color: var(--ink); }
  .lg-freqnum.silent { color: var(--ink-faint); }
  .lg-mhz { font-size: 10.5px; letter-spacing: .1em; color: var(--ink-faint); text-transform: uppercase; }
  .lg-src {
    font-size: 10px; font-weight: 700; letter-spacing: .12em; color: #7a5a24;
    background: rgba(230,162,58,.22); border: 1px solid rgba(180,130,40,.4);
    padding: 2px 6px; border-radius: 3px;
  }
  .lg-src.hd { color: #8a4b00; background: rgba(246,194,90,.30); border-color: rgba(180,120,30,.5); }
  .lg-dash { color: var(--ink-faint); opacity: .6; }
  .lg-name { font-weight: 700; font-size: 15px; color: var(--ink); }
  .lg-callsm { font-size: 11px; color: var(--ink-faint); margin-top: 1px; }
  .lg-np { color: var(--ink-dim); font-style: italic; font-size: 13.5px; }
  .lg-pty { font-size: 12.5px; color: var(--ink-dim); letter-spacing: .02em; }
  .lg-votes { font-weight: 800; font-size: 17px; color: var(--ink-faint); }
  .lg-votes.has { color: var(--needle); }
  .lg-vote {
    font-family: "Barlow Semi Condensed", sans-serif;
    font-size: 12px; font-weight: 700; letter-spacing: .12em; text-transform: uppercase;
    color: #3a2a16; background: linear-gradient(180deg,#f0e0b6,#d8c089);
    border: 1px solid rgba(120,90,40,.5); border-radius: 6px; padding: 6px 14px;
    cursor: pointer; box-shadow: inset 0 1px 0 rgba(255,250,230,.7), 0 1px 2px rgba(0,0,0,.2);
    transition: filter .12s, transform .08s;
  }
  .lg-vote:hover { filter: brightness(1.05); }
  .lg-vote:active { transform: translateY(1px); }
  .lg-vote.unvote { color: #fff; background: linear-gradient(180deg,#e8623f,#c0392b); border: 1px solid #8e2c1a; }

  @media (max-width: 820px) {
    body { padding: 18px 10px 40px; }
    .case { padding: 18px; }
    .plate { gap: 12px; align-items: center; padding: 2px 4px 14px; }
    .brand h1 { font-size: 28px; }
    /* The tuning dots are easy to tap by accident while scrolling on touch
       screens; hide them on mobile — the needle still marks the on-air freq
       and voting happens from the station list below. */
    #dial-lamps { display: none; }
    .pl-toprow { flex-wrap: wrap; gap: 16px; }
    .pl-readout { flex: 1 1 100%; }       /* readout gets its own row… */
    .inst-shell { flex: 1 1 calc(50% - 8px); width: auto; }  /* …meters share the next */
    .pl-station { font-size: 24px; }
    .dial-num { font-size: 16px; }
    .lg-scroll { overflow-x: auto; }      /* tablet band: scroll only if truly needed */
    /* Mobile can't decode the WAV/PCM profiles, so the stream is forced to
       AAC and the quality selector is removed; the lone Signal meter then
       fills the row via flex-grow. */
    .quality-inst { display: none; }
    /* Hide the waveform scope on mobile; let volume take the freed space. */
    .pl-scope { display: none; }
    .pl-volrow { flex: 1; }
    .pl-volrow input { flex: 1; width: auto !important; }
  }
  @media (max-width: 560px) {
    .case { padding: 14px 14px 24px; border-radius: 16px; }   /* clear corner screws */
    .brand h1 { font-size: 22px; }
    .dial-shell { padding: 8px; }
    .dial-glass { height: 132px; }
    .dial-num { font-size: 14px; top: 58px; }
    .pl-wrap { padding: 16px; }
    .pl-station { font-size: 22px; }
    .inst-cap .mono { font-size: 11px; }

    /* Transport: play button + volume slider inline on one row. */
    .pl-controlbar { flex-wrap: wrap; gap: 12px; }
    .pl-divider { display: none; }
    .pl-volrow { flex: 1; gap: 10px; }
    .pl-volrow input { flex: 1; width: auto !important; }

    /* Station log: drop the table grid and render each station as a card. */
    .lg-scroll { overflow-x: hidden; max-height: none; padding-bottom: 4px; }
    .lg-tr:last-child { border-bottom: none; }
    .lg-table { display: block; min-width: 0; }
    .lg-table > tbody { display: block; }
    .lg-table thead { display: none; }
    .lg-tr {
      display: flex; flex-wrap: wrap; align-items: center; column-gap: 10px; row-gap: 4px;
      padding: 13px 14px; border-bottom: 1px solid rgba(120,95,55,.18);
    }
    .lg-td { padding: 0; border: 0; }
    .lg-dash { display: none; }
    .lg-td:has(.lg-dash) { display: none; }   /* collapse empty cells entirely */

    .lg-td:nth-child(1) { order: 1; }                                   /* freq   */
    .lg-td:nth-child(2) { order: 2; }                                   /* src    */
    .lg-td:nth-child(8) { order: 3; margin-left: auto; text-align: right; } /* action */
    .lg-td:nth-child(3) { order: 4; flex-basis: 100%; }                 /* station*/
    .lg-td:nth-child(4) { order: 5; flex-basis: 100%; }                 /* now playing */
    .lg-td:nth-child(5) { order: 6; }                                   /* type   */
    .lg-td:nth-child(6) { order: 7; }                                   /* signal */
    .lg-td:nth-child(7) { order: 8; }                                   /* votes  */

    .lg-freqnum { font-size: 21px; }
    .lg-np { font-size: 14px; }
    /* stats row (type · signal · votes) aligned on one baseline-consistent line */
    .lg-td:nth-child(5), .lg-td:nth-child(6) { display: inline-flex; align-items: center; }
    .lg-td:nth-child(7) { display: inline-flex; align-items: center; gap: 5px; }
    .lg-td:nth-child(7)::before {
      content: "Votes"; font-size: 10px; letter-spacing: .1em;
      text-transform: uppercase; color: var(--ink-faint); font-weight: 700;
    }
    .lg-votes { font-size: 13px; line-height: 1; }
    .lg-vote { padding: 8px 18px; }
  }
</style>
</head>
<body>
<div id="root">
  <div class="case">
    <span class="screw tl"></span><span class="screw tr"></span>
    <span class="screw bl"></span><span class="screw br"></span>

    <div class="plate">
      <div class="brand"><h1>RTL FM</h1><div class="brand-sub" id="brand-sub"></div></div>
      <button class="btn" id="rescan">Rescan&nbsp;Band</button>
    </div>

    <div class="dial-shell">
      <div class="dial-glass" id="dial-glass">
        <div class="dial-backlight"></div>
        <div class="dial-band serif">FM</div>
        <div class="dial-mhz">MHz</div>
        <div id="dial-scale"></div>
        <div id="dial-lamps"></div>
        <div class="dial-needle" id="dial-needle">
          <div class="dial-needle-head"></div>
          <div class="dial-needle-line"></div>
        </div>
      </div>
    </div>

    <div class="pl-wrap">
      <div class="pl-toprow">
        <div class="pl-readout" id="readout"></div>
        <div class="inst-shell quality-inst">
          <div class="inst-face knob-hit" id="knob-face"></div>
          <div class="inst-cap">
            <span class="lbl" style="letter-spacing:.2em">Quality</span>
            <span class="mono" id="q-detail"></span>
          </div>
        </div>
        <div class="inst-shell">
          <div class="inst-face" id="vu-face"></div>
          <div class="inst-cap">
            <span class="lbl" id="vu-label">Signal</span>
            <span class="mono" id="vu-db">0 dB</span>
          </div>
        </div>
      </div>

      <div class="pl-controlbar">
        <button class="pl-playbtn" id="play-btn" aria-label="Play/Pause"></button>
        <div class="pl-scope"><canvas id="scope" style="width:100%;height:100%;display:block"></canvas></div>
        <div class="pl-divider"></div>
        <div class="pl-volrow">
          <svg width="20" height="18" viewBox="0 0 20 18" style="flex-shrink:0">
            <path d="M2 6 h3 l4-3 v12 l-4-3 H2 z" fill="#cdbf9c"></path>
            <path d="M13 5 a5 5 0 0 1 0 8" fill="none" stroke="#e6a23a" stroke-width="1.6" stroke-linecap="round"></path>
            <path d="M15.5 2.5 a9 9 0 0 1 0 13" fill="none" stroke="#e6a23a" stroke-width="1.6" stroke-linecap="round" opacity=".7"></path>
          </svg>
          <input class="slab" id="vol" type="range" min="0" max="100" value="72" style="--fill:72%;width:104px">
        </div>
      </div>
    </div>

    <div class="lg-panel">
      <div class="lg-head">
        <span class="lbl" style="color:var(--ink-dim);letter-spacing:.22em">Station Log</span>
        <span class="lg-hint">Vote for the station you want to hear.</span>
      </div>
      <div class="lg-scroll">
        <table class="lg-table">
          <thead>
            <tr>
              <th class="lg-th">Freq</th>
              <th class="lg-th">Src</th>
              <th class="lg-th">Station</th>
              <th class="lg-th">Now Playing</th>
              <th class="lg-th">Type</th>
              <th class="lg-th center">Signal</th>
              <th class="lg-th center">Votes</th>
              <th class="lg-th"></th>
            </tr>
          </thead>
          <tbody id="rows"><tr><td class="lg-td" colspan="8"><em>waiting for first scan&hellip;</em></td></tr></tbody>
        </table>
      </div>
    </div>
  </div>
</div>

<audio id="player" autoplay style="display:none"></audio>

<script>
// ---------- element refs ----------
const rescanEl   = document.getElementById('rescan');
const glassEl    = document.getElementById('dial-glass');
const scaleEl    = document.getElementById('dial-scale');
const lampsEl    = document.getElementById('dial-lamps');
const needleEl   = document.getElementById('dial-needle');
const readoutEl  = document.getElementById('readout');
const rowsEl     = document.getElementById('rows');
const playerEl   = document.getElementById('player');
const playBtn    = document.getElementById('play-btn');
const volEl      = document.getElementById('vol');
const vuFaceEl   = document.getElementById('vu-face');
const vuDbEl     = document.getElementById('vu-db');
const vuLabelEl  = document.getElementById('vu-label');
const knobFaceEl = document.getElementById('knob-face');
const qDetailEl  = document.getElementById('q-detail');
const scopeCanvas= document.getElementById('scope');

// Cadences. The backend TTL is 12s, so 4s heartbeats survive one drop.
const HEARTBEAT_MS = 4000;
const AUDIO_RECONNECT_MS = 600;
const STREAM_PROFILES = {
  low:    { path: '/stream.aac', detail: 'AAC, 320 kbps' },
  medium: { path: '/stream.wav', detail: '16-bit, 44.1 kHz' },
  high:   { path: '/stream.wav', detail: '24-bit, 192 kHz' },
};
const QUALITY_IDS = ['low', 'medium', 'high'];
// Mobile browsers can't decode the WAV/PCM (medium/high) profiles, so they
// are pinned to AAC and the quality knob is hidden. Matches the CSS breakpoint.
const MOBILE_MQ = window.matchMedia('(max-width: 820px)');
const isMobile = () => MOBILE_MQ.matches;

let BAND = { lo: 88, hi: 108 };
let builtBand = null;

let myFingerprint = null;
let myVote = null;          // freq_hz the user is voting for, or null
let currentWinner = null;   // last known winner freq_hz (for diffing)
let heartbeatTimer = null;
let heartbeatWorker = null;
let reconnectTimer = null;
let playWatchdog = null;
let lastSnapshot = null;
let lastStations = [];       // adapted station shapes
let onAirStation = null;     // adapted winner station
let hoverFreq = null;        // freq_hz hovered in ledger, or null
let scanning = false;
let lastReadoutKey = '';
let liveActive = false;
let streamProfile = localStorage.getItem('rtl-fm-stream-profile') || 'low';
let volume = parseInt(localStorage.getItem('rtl-fm-volume') || '72', 10);
if (!Number.isFinite(volume)) volume = 72;

function freqToPct(fMHz) { return (fMHz - BAND.lo) / (BAND.hi - BAND.lo); }
function fmtFreq(fMHz) { return fMHz.toFixed(1); }
function esc(s) { return (s ?? '').toString().replace(/[<>&"]/g, c => ({'<':'&lt;','>':'&gt;','&':'&amp;','"':'&quot;'}[c])); }

// ---------- snapshot -> design station shape ----------
function snrVal(s) {
  const streaming = s.listeners > 0 || s.freq_hz === currentWinner;
  const live = streaming && s.metadata && Number.isFinite(s.metadata.signal_snr_db);
  const v = live ? s.metadata.signal_snr_db : s.scan_power_db;
  return { v: Math.round(v), live };
}
function adapt(s) {
  const m = s.metadata || null;
  const hd = (m && m.hd) || null;
  const name = (hd && hd.station_name) || (m && (m.ps || m.callsign)) || null;
  let source = null;
  if (hd) source = 'HD';
  else if (m && (m.groups_decoded > 0 || m.pi_hex || m.pty_name)) source = 'RDS';
  let np = null;
  if (hd && (hd.title || hd.artist)) {
    np = [hd.title || '', hd.artist ? '· ' + hd.artist : ''].filter(Boolean).join(' ');
  } else if (m && m.radiotext) {
    np = m.radiotext;
  }
  const { v, live } = snrVal(s);
  return {
    freq_hz: s.freq_hz,
    freq: s.freq_hz / 1e6,
    source,
    name,
    call: (m && m.callsign) || null,
    pi: (m && m.pi_hex) || null,
    np,
    pty: (m && m.pty_name) || null,
    snr: v,
    snrLive: live,
    votes: s.votes,
    listeners: s.listeners,
  };
}

// ---------- dial ----------
// Numerals every 2 MHz on a wide dial; every 4 on narrow screens so they
// don't collide (the band is fixed-width but the dial isn't).
function numeralStep() { return window.innerWidth < 560 ? 4 : 2; }
function dialKey() { return BAND.lo + '/' + BAND.hi + '/' + numeralStep(); }
function buildDialScale() {
  const step = numeralStep();
  let html = '';
  for (let f = BAND.lo; f <= BAND.hi + 1e-6; f += 0.5) {
    const isMaj = Math.abs(f % 2) < 0.001;
    html += `<div class="dial-tick" style="left:${(freqToPct(f) * 100).toFixed(3)}%;height:${isMaj ? 16 : 8}px;opacity:${isMaj ? 0.85 : 0.5}"></div>`;
  }
  const start = Math.ceil((BAND.lo + 1) / step) * step;
  const end = Math.floor((BAND.hi - 1) / step) * step;
  for (let f = start; f <= end; f += step) {
    html += `<div class="dial-num">${f}</div>`;
  }
  scaleEl.innerHTML = html;
  // numerals need positioning after insertion (template can't carry computed left cleanly)
  let i = 0;
  scaleEl.querySelectorAll('.dial-num').forEach(el => {
    el.style.left = (freqToPct(start + i * step) * 100).toFixed(3) + '%';
    i++;
  });
  builtBand = dialKey();
}

function updateLamps(stations, winner, vote) {
  lampsEl.innerHTML = stations.map(s => {
    const active = s.source != null;
    const isOnAir = s.freq_hz === winner;
    const isVote = s.freq_hz === vote;
    const lit = isOnAir || isVote;
    const bg = lit ? (isOnAir ? 'var(--onair)' : 'var(--vote-lamp)')
                   : active ? 'var(--amber)' : 'rgba(120,100,70,.45)';
    const sh = lit ? (isOnAir ? '0 0 11px 2px var(--onair)' : '0 0 10px 2px rgba(95,184,122,.85)')
                   : active ? '0 0 6px rgba(230,162,58,.6)' : 'none';
    const title = (s.name ? esc(s.name) + ' · ' : '') + s.freq.toFixed(1) + ' MHz' + (active ? '' : ' (no signal)');
    return `<button class="dial-lamp" data-vote="${s.freq_hz}" title="${title}" style="left:${(freqToPct(s.freq) * 100).toFixed(3)}%;background:${bg};box-shadow:${sh};transform:translateX(-50%) scale(${lit ? 1.35 : 1})"></button>`;
  }).join('');
}

function needlePctFor(fMHz) { return Math.max(0, Math.min(100, freqToPct(fMHz) * 100)); }
function updateNeedle(fMHz, animate) {
  needleEl.style.transition = animate ? 'left .7s cubic-bezier(.5,.05,.2,1)' : 'none';
  needleEl.style.left = needlePctFor(fMHz) + '%';
}

// ---------- VU meter ----------
const VU = { A0: -54, A1: 54, PIV: [100, 112] };
const vuDeg   = db => VU.A0 + (db / 50) * (VU.A1 - VU.A0);
const vuRad   = db => vuDeg(db) * Math.PI / 180;
const vuPolar = (db, r) => [VU.PIV[0] + Math.sin(vuRad(db)) * r, VU.PIV[1] - Math.cos(vuRad(db)) * r];
function vuArc(d0, d1, r) {
  const [x0, y0] = vuPolar(d0, r), [x1, y1] = vuPolar(d1, r);
  return `M${x0.toFixed(2)} ${y0.toFixed(2)} A ${r} ${r} 0 0 1 ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}
function buildVu() {
  const majors = [0, 10, 20, 30, 40, 50], minors = [5, 15, 25, 35, 45];
  let ticks = '';
  minors.forEach(t => {
    const [x1, y1] = vuPolar(t, 79), [x2, y2] = vuPolar(t, 88);
    ticks += `<line x1="${x1.toFixed(2)}" y1="${y1.toFixed(2)}" x2="${x2.toFixed(2)}" y2="${y2.toFixed(2)}" stroke="#5a4528" stroke-width="1" opacity="0.7"/>`;
  });
  majors.forEach(t => {
    const [x1, y1] = vuPolar(t, 76), [x2, y2] = vuPolar(t, 89), [tx, ty] = vuPolar(t, 64);
    const hot = t >= 40;
    ticks += `<line x1="${x1.toFixed(2)}" y1="${y1.toFixed(2)}" x2="${x2.toFixed(2)}" y2="${y2.toFixed(2)}" stroke="${hot ? '#a8331f' : '#3a2a16'}" stroke-width="1.8"/>`;
    ticks += `<text x="${tx.toFixed(2)}" y="${(ty + 3.2).toFixed(2)}" text-anchor="middle" font-size="9.5" font-family="'DM Mono', monospace" font-weight="500" fill="${hot ? '#a8331f' : '#4a3520'}">${t}</text>`;
  });
  const P = VU.PIV;
  vuFaceEl.innerHTML = `<svg viewBox="0 0 200 124" style="height:90px;width:auto;max-width:100%;display:block">
    <defs>
      <radialGradient id="vuHub" cx="38%" cy="32%" r="75%">
        <stop offset="0%" stop-color="#7a6446"/><stop offset="55%" stop-color="#3a2c1a"/><stop offset="100%" stop-color="#1c140c"/>
      </radialGradient>
      <radialGradient id="vuSheen" cx="50%" cy="42%" r="58%">
        <stop offset="0%" stop-color="#fffdf3" stop-opacity="0.4"/><stop offset="100%" stop-color="#fffdf3" stop-opacity="0"/>
      </radialGradient>
    </defs>
    <path d="${vuArc(0, 50, 84)}" fill="none" stroke="#cdbb92" stroke-width="9" stroke-linecap="round"/>
    <path d="${vuArc(0, 50, 84)}" fill="none" stroke="#4a3a22" stroke-width="1.4"/>
    <path d="${vuArc(38, 50, 84)}" fill="none" stroke="#c0392b" stroke-width="4.4" stroke-linecap="round" opacity="0.92"/>
    ${ticks}
    <g class="vu-needle" id="vu-needle" style="transform-origin:${P[0]}px ${P[1]}px">
      <path d="M${P[0]} 26 L${P[0] - 2.2} ${P[1]} L${P[0] + 2.2} ${P[1]} Z" fill="#241a10"/>
      <line x1="${P[0]}" y1="26" x2="${P[0]}" y2="44" stroke="#c0392b" stroke-width="2.4" stroke-linecap="round"/>
      <rect x="${P[0] - 1.6}" y="${P[1]}" width="3.2" height="9" rx="1.4" fill="#241a10"/>
    </g>
    <circle cx="${P[0]}" cy="${P[1]}" r="7.5" fill="url(#vuHub)" stroke="#120c07" stroke-width="1"/>
    <circle cx="${P[0] - 1.6}" cy="${P[1] - 2}" r="1.8" fill="#b59a6e" opacity="0.8"/>
    <ellipse cx="100" cy="6" rx="96" ry="34" fill="url(#vuSheen)" pointer-events="none"/>
  </svg>`;
}
function updateVu(snr, label) {
  const c = Math.max(0, Math.min(50, snr || 0));
  const g = document.getElementById('vu-needle');
  if (g) g.style.transform = `rotate(${vuDeg(c)}deg)`;
  vuDbEl.textContent = Math.round(c) + ' dB';
  vuLabelEl.textContent = label || 'Signal';
}

// ---------- quality knob ----------
const KNOB = { C: [42, 46], ang: { low: -58, medium: 0, high: 58 } };
const kpol = (a, r) => [KNOB.C[0] + Math.sin(a * Math.PI / 180) * r, KNOB.C[1] - Math.cos(a * Math.PI / 180) * r];
function buildKnob() {
  const C = KNOB.C;
  const stops = [{ id: 'low', t: 'LO', a: -58 }, { id: 'medium', t: 'MED', a: 0 }, { id: 'high', t: 'HI', a: 58 }];
  let stopMarks = '';
  stops.forEach(s => {
    const [x1, y1] = kpol(s.a, 30), [x2, y2] = kpol(s.a, 34), [lx, ly] = kpol(s.a, 40);
    stopMarks += `<line id="kline-${s.id}" x1="${x1.toFixed(2)}" y1="${y1.toFixed(2)}" x2="${x2.toFixed(2)}" y2="${y2.toFixed(2)}" stroke="#8a7458" stroke-width="2" stroke-linecap="round"/>`;
    stopMarks += `<text id="ktext-${s.id}" x="${lx.toFixed(2)}" y="${(ly + 2.8).toFixed(2)}" text-anchor="middle" font-size="7.5" font-family="'Barlow Semi Condensed',sans-serif" font-weight="700" letter-spacing="0.5" fill="#6b5d49">${s.t}</text>`;
  });
  let knurl = '';
  for (let i = 0; i < 44; i++) {
    const a = i * (360 / 44);
    const [x1, y1] = kpol(a, 24.5), [x2, y2] = kpol(a, 27.5);
    knurl += `<line x1="${x1.toFixed(2)}" y1="${y1.toFixed(2)}" x2="${x2.toFixed(2)}" y2="${y2.toFixed(2)}" stroke="#473a28" stroke-width="1" opacity="0.55"/>`;
  }
  knobFaceEl.innerHTML = `<div title="Stream quality — drag to turn or click to advance" style="touch-action:none">
    <svg viewBox="0 0 84 86" style="height:90px;width:auto;display:block">
      <defs>
        <radialGradient id="qkMetal" cx="37%" cy="29%" r="74%">
          <stop offset="0%" stop-color="#f3ecdd"/><stop offset="38%" stop-color="#cbbfa6"/>
          <stop offset="72%" stop-color="#9b8c70"/><stop offset="100%" stop-color="#69593f"/>
        </radialGradient>
        <linearGradient id="qkSheen" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stop-color="#ffffff" stop-opacity="0.5"/>
          <stop offset="46%" stop-color="#ffffff" stop-opacity="0"/>
          <stop offset="100%" stop-color="#000000" stop-opacity="0.26"/>
        </linearGradient>
      </defs>
      ${stopMarks}
      ${knurl}
      <circle cx="${C[0]}" cy="${C[1]}" r="24" fill="url(#qkMetal)" stroke="#1a130b" stroke-width="1.4"/>
      <circle cx="${C[0]}" cy="${C[1]}" r="24" fill="url(#qkSheen)"/>
      <circle cx="${C[0]}" cy="${C[1] - 0.7}" r="24" fill="none" stroke="rgba(255,255,255,.4)" stroke-width="0.8"/>
      <g class="q-pointer" id="q-pointer" style="transform-origin:${C[0]}px ${C[1]}px">
        <line x1="${C[0]}" y1="${C[1]}" x2="${C[0]}" y2="${C[1] - 22}" stroke="#1c150d" stroke-width="3.4" stroke-linecap="round"/>
        <line x1="${C[0] + 1}" y1="${C[1] - 2}" x2="${C[0] + 1}" y2="${C[1] - 21}" stroke="rgba(255,255,255,.28)" stroke-width="0.8" stroke-linecap="round"/>
      </g>
      <circle cx="${C[0]}" cy="${C[1]}" r="2.6" fill="#241a10"/>
    </svg>
  </div>`;
}
function updateKnob() {
  const g = document.getElementById('q-pointer');
  if (g) g.style.transform = `rotate(${KNOB.ang[streamProfile] ?? 0}deg)`;
  QUALITY_IDS.forEach(id => {
    const on = id === streamProfile;
    const line = document.getElementById('kline-' + id);
    const text = document.getElementById('ktext-' + id);
    if (line) line.setAttribute('stroke', on ? '#b5471f' : '#8a7458');
    if (text) text.setAttribute('fill', on ? '#b5471f' : '#6b5d49');
  });
  const p = STREAM_PROFILES[streamProfile] || STREAM_PROFILES.low;
  qDetailEl.textContent = p.detail;
}
function setQuality(profile) {
  if (isMobile()) return; // pinned to AAC on mobile
  if (!STREAM_PROFILES[profile]) return;
  streamProfile = profile;
  localStorage.setItem('rtl-fm-stream-profile', profile);
  updateKnob();
  // Re-open the stream at the new quality. If the user has paused, just stash
  // the profile — the next Play picks it up via streamUrl().
  if (currentWinner != null && !playerEl.paused) startAudio();
}
// Crossing into the mobile breakpoint (e.g. a desktop window shrunk down)
// pins the stream to AAC.
MOBILE_MQ.addEventListener('change', e => {
  if (e.matches && streamProfile !== 'low') {
    streamProfile = 'low';
    updateKnob();
    if (currentWinner != null) startAudio();
  }
});
// knob interaction (drag to turn / click to advance)
(function wireKnob() {
  let dragging = false, moved = false;
  const getDeg = (e) => {
    const r = knobFaceEl.getBoundingClientRect();
    const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
    return Math.atan2(e.clientX - cx, -(e.clientY - cy)) * 180 / Math.PI;
  };
  const setFrom = (deg) => {
    const c = Math.max(-58, Math.min(58, deg));
    const idx = Math.max(0, Math.min(2, Math.round((c + 58) / 58)));
    setQuality(QUALITY_IDS[idx]);
  };
  knobFaceEl.addEventListener('pointerdown', e => { e.preventDefault(); dragging = true; moved = false; });
  window.addEventListener('pointermove', e => { if (dragging) { moved = true; setFrom(getDeg(e)); } });
  window.addEventListener('pointerup', () => { dragging = false; });
  knobFaceEl.addEventListener('click', () => {
    if (moved) return;
    const i = QUALITY_IDS.indexOf(streamProfile);
    setQuality(QUALITY_IDS[(i + 1) % 3]);
  });
})();

// ---------- live audio analyser ----------
// Routes the <audio> element through a Web Audio AnalyserNode so the scope
// can draw the actual signal the browser is playing. Created lazily inside a
// user gesture (first Vote/Play) because AudioContext starts suspended under
// autoplay policy. createMediaElementSource may only be called once per
// element, so we guard with `analyser`.
let audioCtx = null, analyser = null, srcNode = null, gainNode = null, timeData = null;
let liveLevel = 0;        // VU value (0..50) measured from the live stream
let analyserLive = false; // true when the analyser is delivering real signal
function resumeCtx() {
  if (audioCtx && audioCtx.state !== 'running') audioCtx.resume().catch(() => {});
}
function ensureAnalyser() {
  if (analyser) { resumeCtx(); return; }
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return; // no Web Audio -> synthetic fallback + element.volume
  try {
    audioCtx = new AC();
    srcNode = audioCtx.createMediaElementSource(playerEl);
    // Element routed through Web Audio no longer honours element.volume, so a
    // GainNode owns the volume from here on (see applyVolume).
    gainNode = audioCtx.createGain();
    gainNode.gain.value = volume / 100;
    analyser = audioCtx.createAnalyser();
    analyser.fftSize = 2048;
    analyser.smoothingTimeConstant = 0.6;
    timeData = new Uint8Array(analyser.fftSize);
    srcNode.connect(gainNode);
    gainNode.connect(analyser);
    analyser.connect(audioCtx.destination); // keep audio audible
    playerEl.volume = 1;
    resumeCtx(); // mobile: a fresh context starts suspended
  } catch (e) {
    analyser = null; gainNode = null;
    console.warn('audio analyser unavailable, using synthetic scope:', e);
  }
}
// Volume is owned by the GainNode when the Web Audio graph is up, else by the
// media element directly.
function applyVolume() {
  if (gainNode) { gainNode.gain.value = volume / 100; playerEl.volume = 1; }
  else playerEl.volume = volume / 100;
}

// ---------- live waveform scope ----------
(function liveWave() {
  const ctx = scopeCanvas.getContext('2d');
  const dpr = Math.min(2, window.devicePixelRatio || 1);
  let w = 0, h = 0;
  const resize = () => {
    const r = scopeCanvas.getBoundingClientRect();
    w = r.width || 300; h = r.height || 44;
    scopeCanvas.width = w * dpr; scopeCanvas.height = h * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  };
  resize();
  if (window.ResizeObserver) new ResizeObserver(resize).observe(scopeCanvas);
  else window.addEventListener('resize', resize);
  const root = document.documentElement;
  function draw() {
    ctx.clearRect(0, 0, w, h);
    const col = (getComputedStyle(root).getPropertyValue('--onair') || '#ef5230').trim();
    const mid = h / 2;
    // centre guideline
    ctx.strokeStyle = 'rgba(255,236,200,.06)'; ctx.lineWidth = 1; ctx.shadowBlur = 0;
    ctx.beginPath(); ctx.moveTo(0, mid); ctx.lineTo(w, mid); ctx.stroke();

    ctx.lineWidth = 2; ctx.strokeStyle = col; ctx.shadowColor = col;

    if (analyser) {
      // Real audio only: time-domain samples (0..255 around 128). When the
      // stream is silent or paused this draws a flat line — there is no
      // synthetic/fake waveform.
      analyser.getByteTimeDomainData(timeData);
      const N = timeData.length;
      let peak = 0, sumSq = 0;
      ctx.beginPath();
      for (let x = 0; x <= w; x++) {
        const i = (x / w) * (N - 1);
        const i0 = i | 0;
        const frac = i - i0;
        const s = timeData[i0] + (timeData[Math.min(N - 1, i0 + 1)] - timeData[i0]) * frac;
        const v = (s - 128) / 128;
        if (Math.abs(v) > peak) peak = Math.abs(v);
        sumSq += v * v;
        const y = mid - v * mid * 0.92;
        x === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
      }
      ctx.shadowBlur = 2 + 10 * Math.min(1, peak * 1.6);
      ctx.stroke();

      analyserLive = peak > 0.004;
      // Signal gauge: RMS of the live stream -> dB -> 0..50 gauge scale.
      const rms = Math.sqrt(sumSq / N);
      const db = 20 * Math.log10(rms + 1e-6);
      const gauge = Math.max(0, Math.min(50, (db + 60) / 60 * 50));
      liveLevel += (gauge - liveLevel) * 0.2;
      if (hoverFreq == null && analyserLive) updateVu(liveLevel, 'Signal');
    } else {
      // No Web Audio support at all: a flat idle line, never a fake wave.
      analyserLive = false; liveLevel = 0;
      ctx.shadowBlur = 2;
      ctx.beginPath(); ctx.moveTo(0, mid); ctx.lineTo(w, mid); ctx.stroke();
    }
    requestAnimationFrame(draw);
  }
  draw();
})();

// ---------- player readout ----------
function readoutKey(o) {
  if (!o) return 'silent';
  return [o.freq_hz, o.name, o.np, o.call, o.pi, o.pty, o.listeners].join('|');
}
function buildReadout(o) {
  if (!o) {
    return `<div class="serif pl-station" style="color:var(--ink-faint);font-size:24px">Nothing playing</div>
      <div class="pl-np" style="color:var(--ink-faint);height:auto;margin-top:6px">Pick a station below and hit Vote to put it on the air.</div>`;
  }
  const named = !!o.name;
  const nameTxt = named ? o.name : (o.freq.toFixed(1) + ' FM');
  const freqTag = named
    ? `<span class="mono pl-freqtag">${o.freq.toFixed(1)}<span class="pl-frequnit">MHz</span></span>`
    : '';
  const callLead = o.call ? `${esc(o.call)} · PI ${esc(o.pi || '')}` : 'Unnamed station';
  const callLine = `${callLead} · ${o.listeners} listener${o.listeners === 1 ? '' : 's'}`;
  let np;
  if (o.np) {
    np = `<div class="pl-np"><span class="serif pl-nptext" title="${esc(o.np)}">${esc(o.np)}</span></div>`;
  } else {
    const txt = o.pty ? esc(o.pty) + ' · no song info' : 'No song info';
    np = `<div class="pl-np"><span class="serif" style="color:var(--ink-faint);font-style:italic">${txt}</span></div>`;
  }
  return `<div class="pl-headline"><span class="serif pl-station">${esc(nameTxt)}</span>${freqTag}</div>
    <div class="mono pl-call">${callLine}</div>${np}`;
}

function updatePlayGlyph() {
  const silence = currentWinner == null;
  playBtn.disabled = silence;
  playBtn.style.opacity = silence ? 0.45 : 1;
  playBtn.style.cursor = silence ? 'default' : 'pointer';
  const live = !silence && !playerEl.paused;
  playBtn.innerHTML = live
    ? '<span class="pl-pauseglyph"><span class="pl-pausebar"></span><span class="pl-pausebar"></span></span>'
    : '<span class="pl-playglyph"></span>';
  liveActive = live;
}

// ---------- ledger ----------
function sigBars(snr) {
  const lvl = Math.max(0, Math.min(5, Math.round(snr / 50 * 5)));
  let bars = '';
  for (let b = 1; b <= 5; b++) {
    const col = b <= lvl ? (snr < 22 ? 'var(--needle)' : 'var(--amber)') : 'rgba(120,100,70,.3)';
    bars += `<span style="width:3px;height:${3 + b * 2}px;background:${col};border-radius:1px"></span>`;
  }
  return `<span style="display:inline-flex;align-items:center;gap:6px">
    <span style="display:inline-flex;align-items:flex-end;gap:2px;height:14px">${bars}</span>
    <span class="mono" style="font-size:12px;color:var(--ink-dim);white-space:nowrap">${snr} dB</span>
  </span>`;
}

function render(snap) {
  lastSnapshot = snap;
  if (Array.isArray(snap.scan_band_mhz) && snap.scan_band_mhz.length === 2) {
    BAND = { lo: snap.scan_band_mhz[0], hi: snap.scan_band_mhz[1] };
  }
  const winner = snap.winner_hz ?? null;
  const idleFreq = snap.idle_refresher_freq_hz ?? null;
  const stations = (snap.stations || []).map(adapt);
  lastStations = stations;
  onAirStation = winner != null ? stations.find(s => s.freq_hz === winner) || null : null;

  // audio: restart on winner change so we pick up the new station.
  if (winner !== currentWinner) {
    currentWinner = winner;
    if (winner != null) startAudio(); else stopAudio();
  } else if (winner != null && playerEl.paused && !playerEl.currentSrc) {
    startAudio();
  }

  // dial
  if (builtBand !== dialKey()) buildDialScale();
  updateLamps(stations, winner, myVote);
  if (!scanning) {
    const nf = winner != null ? winner / 1e6 : (BAND.lo + BAND.hi) / 2;
    updateNeedle(nf, true);
  }

  // player readout (rebuild only when the on-air content actually changes)
  const key = readoutKey(onAirStation);
  if (key !== lastReadoutKey) {
    readoutEl.innerHTML = buildReadout(onAirStation);
    lastReadoutKey = key;
  }
  updatePlayGlyph();

  // VU meter follows the hovered row, else the on-air station
  refreshVu();

  // ledger
  rowsEl.innerHTML = stations.map(s => {
    const isOnAir = s.freq_hz === winner;
    const isVote = s.freq_hz === myVote;
    const isRefreshing = s.freq_hz === idleFreq;
    const cls = ['lg-tr', isOnAir ? 'on' : '', isRefreshing ? 'refreshing' : ''].filter(Boolean).join(' ');
    const srcCls = s.source === 'HD' ? 'lg-src hd' : 'lg-src';
    const src = s.source ? `<span class="${srcCls}">${s.source}</span>` : '<span class="lg-dash">—</span>';
    const station = s.name
      ? `<div><div class="serif lg-name">${esc(s.name)}</div>${s.call ? `<div class="mono lg-callsm">${esc(s.call)} · PI ${esc(s.pi || '')}</div>` : ''}</div>`
      : '<span class="lg-dash">—</span>';
    const np = s.np ? `<span class="serif lg-np">${esc(s.np)}</span>` : '<span class="lg-dash">—</span>';
    const pty = s.pty ? `<span class="lg-pty">${esc(s.pty)}</span>` : '<span class="lg-dash">—</span>';
    const voteBtn = isVote
      ? `<button class="lg-vote unvote" data-unvote="1">Unvote</button>`
      : `<button class="lg-vote" data-vote="${s.freq_hz}">Vote</button>`;
    return `<tr class="${cls}" data-freq="${s.freq_hz}">
      <td class="lg-td lg-tdfreq"><span class="serif lg-freqnum${s.source ? '' : ' silent'}">${s.freq.toFixed(1)}</span><span class="lg-mhz"> MHz</span></td>
      <td class="lg-td">${src}</td>
      <td class="lg-td">${station}</td>
      <td class="lg-td" style="max-width:280px">${np}</td>
      <td class="lg-td">${pty}</td>
      <td class="lg-td center" style="white-space:nowrap">${sigBars(s.snr)}</td>
      <td class="lg-td center"><span class="serif lg-votes${s.votes > 0 ? ' has' : ''}">${s.votes}</span></td>
      <td class="lg-td" style="text-align:right">${voteBtn}</td>
    </tr>`;
  }).join('') || '<tr><td class="lg-td" colspan="8"><em>no stations yet — try Rescan Band</em></td></tr>';
}

function refreshVu() {
  // Hovering a row probes that station's metadata SNR; otherwise the gauge
  // reads the live stream level (driven each frame by the analyser), falling
  // back to the on-air station's metadata SNR when nothing is playing.
  if (hoverFreq != null) {
    const h = lastStations.find(s => s.freq_hz === hoverFreq);
    if (h) { updateVu(h.snr, h.freq.toFixed(1)); return; }
  }
  if (analyserLive) { updateVu(liveLevel, 'Signal'); return; }
  updateVu(onAirStation ? onAirStation.snr : 0, 'Signal');
}

// ---------- audio ----------
function streamUrl() {
  const p = STREAM_PROFILES[streamProfile] || STREAM_PROFILES.low;
  // fp lets the server count this listener toward /meta daily listeners.
  const fp = encodeURIComponent(myFingerprint || '');
  return `${p.path}?profile=${encodeURIComponent(streamProfile)}&fp=${fp}&t=${Date.now()}`;
}
// The medium/high profiles stream PCM WAV, which many mobile browsers can't
// play; only the AAC `low` profile is universally supported. Drop to it so
// playback is never silently broken.
function fallbackToLow() {
  if (streamProfile === 'low') return;
  console.warn('stream profile unsupported here; falling back to low (AAC)');
  streamProfile = 'low';
  localStorage.setItem('rtl-fm-stream-profile', 'low');
  updateKnob();
  if (currentWinner != null) startAudio();
}

function startAudio() {
  ensureAnalyser();
  playerEl.src = streamUrl();
  applyVolume();
  // Watchdog: if a hi-fi (WAV) profile produces no audio frames shortly after
  // play, this browser can't decode it — fall back to AAC.
  clearTimeout(playWatchdog);
  if (streamProfile !== 'low') {
    playWatchdog = setTimeout(() => {
      if (currentWinner != null && streamProfile !== 'low' && playerEl.currentTime === 0) {
        fallbackToLow();
      }
    }, 4000);
  }
  const p = playerEl.play();
  if (p && p.catch) p.catch(err => console.warn('audio.play() blocked:', err && err.name));
}
function stopAudio() {
  playerEl.pause();
  playerEl.removeAttribute('src');
  playerEl.load();
}
playerEl.addEventListener('error', () => {
  const err = playerEl.error;
  // Decode/format error (codes 3=decode, 4=src-not-supported) on a hi-fi WAV
  // profile this browser can't play — switch to AAC rather than retry.
  if (err && (err.code === 3 || err.code === 4) && streamProfile !== 'low') {
    fallbackToLow();
    return;
  }
  // Otherwise the server likely dropped us (winner changed) — retry the same.
  clearTimeout(reconnectTimer);
  reconnectTimer = setTimeout(() => { if (currentWinner != null) startAudio(); }, AUDIO_RECONNECT_MS);
});
playerEl.addEventListener('ended', () => {
  clearTimeout(reconnectTimer);
  reconnectTimer = setTimeout(() => { if (currentWinner != null) startAudio(); }, AUDIO_RECONNECT_MS);
});
playerEl.addEventListener('play', () => { resumeCtx(); updatePlayGlyph(); });
playerEl.addEventListener('playing', () => { clearTimeout(playWatchdog); resumeCtx(); });
playerEl.addEventListener('pause', updatePlayGlyph);

// Mobile browsers create the AudioContext suspended and only let it resume
// from a user gesture; until it runs, the analyser reads pure silence. Build
// and resume the graph on the first interaction so it captures real samples
// (rather than ever falling back to a fake waveform).
['pointerdown', 'touchend'].forEach(ev =>
  window.addEventListener(ev, () => { ensureAnalyser(); resumeCtx(); }, { passive: true }));

playBtn.addEventListener('click', () => {
  if (currentWinner == null) return;
  ensureAnalyser();
  if (playerEl.paused) startAudio(); else playerEl.pause();
});
volEl.value = volume;
volEl.style.setProperty('--fill', volume + '%');
applyVolume();
volEl.addEventListener('input', () => {
  volume = Number(volEl.value);
  volEl.style.setProperty('--fill', volume + '%');
  applyVolume();
  localStorage.setItem('rtl-fm-volume', String(volume));
});

function showError(msg) {
  console.error('rtl-fm vote:', msg);
}

// ---------- voting ----------
async function castVote(freqHz) {
  if (!myFingerprint) { showError('cannot vote: fingerprint not initialised'); return; }
  const previousVote = myVote;
  myVote = freqHz;
  if (lastSnapshot) render(lastSnapshot);
  // kick audio inside the user gesture so the browser unlocks playback
  startAudio();
  const ok = await sendHeartbeat();
  if (!ok) { myVote = previousVote; if (lastSnapshot) render(lastSnapshot); return; }
  scheduleHeartbeat();
}
async function dropVote() {
  if (myVote == null) return;
  const fp = myFingerprint;
  myVote = null;
  if (heartbeatTimer) { clearTimeout(heartbeatTimer); heartbeatTimer = null; }
  if (heartbeatWorker) heartbeatWorker.postMessage('stop');
  if (lastSnapshot) render(lastSnapshot);
  try {
    await fetch('/api/unvote', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ fingerprint: fp }), keepalive: true,
    });
  } catch (e) { console.warn('unvote failed', e); }
}
async function sendHeartbeat() {
  if (myVote == null || !myFingerprint) return false;
  try {
    const r = await fetch('/api/vote', {
      method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ fingerprint: myFingerprint, freq_hz: myVote }), keepalive: true,
    });
    if (!r.ok) {
      const text = await r.text().catch(() => '');
      showError(`vote failed: ${r.status} ${text}`);
      return false;
    }
    return true;
  } catch (e) { showError('vote request failed: ' + e); return false; }
}
function startHeartbeatWorker() {
  if (heartbeatWorker || !window.Worker || !window.Blob || !window.URL) return;
  const src = `let timer=null;onmessage=e=>{if(e.data==='start'){clearInterval(timer);timer=setInterval(()=>postMessage('beat'),${HEARTBEAT_MS});}if(e.data==='stop'){clearInterval(timer);timer=null;}};`;
  try {
    heartbeatWorker = new Worker(URL.createObjectURL(new Blob([src], { type: 'text/javascript' })));
    heartbeatWorker.onmessage = () => { if (myVote != null) sendHeartbeat(); };
  } catch (e) { heartbeatWorker = null; }
}
function scheduleHeartbeat() {
  if (heartbeatTimer) clearTimeout(heartbeatTimer);
  startHeartbeatWorker();
  if (myVote == null) return;
  if (heartbeatWorker) heartbeatWorker.postMessage('start');
  heartbeatTimer = setTimeout(async () => { await sendHeartbeat(); scheduleHeartbeat(); }, HEARTBEAT_MS);
}

document.addEventListener('visibilitychange', () => {
  if (!document.hidden && myVote != null) sendHeartbeat().then(scheduleHeartbeat);
});
window.addEventListener('pagehide', () => {
  if (myVote != null && myFingerprint) {
    if (heartbeatWorker) heartbeatWorker.postMessage('stop');
    try {
      fetch('/api/unvote', {
        method: 'POST', headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ fingerprint: myFingerprint }), keepalive: true,
      });
    } catch {}
  }
});

// ---------- rescan (sweeps the needle while the backend re-reads) ----------
function rescan() {
  if (scanning) return;
  scanning = true;
  rescanEl.disabled = true;
  rescanEl.textContent = 'Scanning…';
  glassEl.classList.add('scanning');
  const start = performance.now(), DUR = 1500;
  function step(now) {
    if (!scanning) return;
    const p = Math.min(1, (now - start) / DUR);
    const tri = p < 0.5 ? p * 2 : (1 - p) * 2;
    updateNeedle(BAND.lo + (BAND.hi - BAND.lo) * tri, false);
    if (p < 1) requestAnimationFrame(step);
  }
  requestAnimationFrame(step);
  fetch('/api/rescan').catch(() => {}).finally(() => {
    scanning = false;
    rescanEl.disabled = false;
    rescanEl.textContent = 'Rescan Band';
    glassEl.classList.remove('scanning');
    if (lastSnapshot) render(lastSnapshot);
  });
}

// ---------- delegated clicks ----------
document.addEventListener('click', async ev => {
  const voteBtn = ev.target.closest('[data-vote]');
  if (voteBtn) {
    const f = parseInt(voteBtn.dataset.vote, 10);
    if (Number.isFinite(f)) await castVote(f);
    return;
  }
  if (ev.target.closest('[data-unvote]')) { await dropVote(); return; }
  if (ev.target.closest('#rescan')) rescan();
});

// ledger hover drives the VU meter
rowsEl.addEventListener('mouseover', ev => {
  const tr = ev.target.closest('tr[data-freq]');
  if (!tr) return;
  const f = parseInt(tr.dataset.freq, 10);
  if (Number.isFinite(f) && f !== hoverFreq) { hoverFreq = f; refreshVu(); }
});
rowsEl.addEventListener('mouseleave', () => { hoverFreq = null; refreshVu(); });

// ---------- auth / role ----------
// The admin token arrives as ?token=… and the server has already stored it
// as a cookie on this page load; drop it from the address bar so it isn't
// bookmarked or shared. Then ask the server what role this browser has.
function stripAdminToken() {
  try {
    const u = new URL(window.location.href);
    if (u.searchParams.has('token')) {
      u.searchParams.delete('token');
      const qs = u.searchParams.toString();
      window.history.replaceState({}, '', u.pathname + (qs ? '?' + qs : '') + u.hash);
    }
  } catch (e) { /* ignore */ }
}
async function applyRole() {
  try {
    const r = await fetch('/api/whoami');
    if (r.ok && (await r.json()).admin) document.body.classList.add('is-admin');
  } catch (e) { /* ignore */ }
}

// ---------- site meta ----------
async function loadMeta() {
  try {
    const r = await fetch('/meta');
    if (!r.ok) return;
    const m = await r.json();
    const sub = document.getElementById('brand-sub');
    if (sub) {
      const parts = [m.owner, m.location].map(s => (s || '').trim()).filter(Boolean);
      sub.textContent = parts.join(' · ');
    }
  } catch (e) { /* ignore */ }
}

// ---------- SSE ----------
function openSse() {
  const es = new EventSource('/api/stations/stream');
  es.onmessage = ev => { try { render(JSON.parse(ev.data)); } catch (e) { /* ignore */ } };
  es.onerror = () => { /* browser auto-reconnects */ };
  return es;
}

// ---------- fingerprinting ----------
async function computeFingerprint() {
  const cached = localStorage.getItem('rtl-fm-fp');
  if (cached && cached.indexOf('-') !== -1) return cached;

  const bits = [];
  try {
    const c = document.createElement('canvas');
    c.width = 280; c.height = 60;
    const ctx = c.getContext('2d');
    ctx.textBaseline = 'top';
    ctx.font = "14px 'Arial'";
    ctx.fillStyle = '#f60';
    ctx.fillRect(0, 0, 280, 60);
    ctx.fillStyle = '#069';
    ctx.fillText('rtl-fm vote \u{1F4FB}', 4, 4);
    ctx.fillStyle = 'rgba(102, 204, 0, 0.7)';
    ctx.fillText('the quick brown fox', 4, 26);
    bits.push(c.toDataURL());
  } catch (e) { bits.push('canvas-err'); }

  try {
    const gl = document.createElement('canvas').getContext('webgl')
      || document.createElement('canvas').getContext('experimental-webgl');
    if (gl) {
      const dbg = gl.getExtension('WEBGL_debug_renderer_info');
      const renderer = dbg ? gl.getParameter(dbg.UNMASKED_RENDERER_WEBGL) : gl.getParameter(gl.RENDERER);
      const vendor = dbg ? gl.getParameter(dbg.UNMASKED_VENDOR_WEBGL) : gl.getParameter(gl.VENDOR);
      bits.push(renderer + '||' + vendor);
    }
  } catch (e) { bits.push('webgl-err'); }

  try {
    const AC = window.OfflineAudioContext || window.webkitOfflineAudioContext;
    if (AC) {
      const ac = new AC(1, 4410, 44100);
      const osc = ac.createOscillator();
      osc.type = 'triangle';
      osc.frequency.value = 1000;
      const comp = ac.createDynamicsCompressor();
      osc.connect(comp); comp.connect(ac.destination);
      osc.start(0);
      const buf = await ac.startRendering();
      const data = buf.getChannelData(0);
      let acc = 0;
      for (let i = 0; i < data.length; i += 100) acc += Math.abs(data[i]);
      bits.push(acc.toFixed(8));
    }
  } catch (e) { bits.push('audio-err'); }

  bits.push(navigator.userAgent || '');
  bits.push(navigator.language || '');
  bits.push((navigator.languages || []).join(','));
  bits.push(screen.colorDepth || 0);
  bits.push(`${screen.width}x${screen.height}`);
  bits.push(new Date().getTimezoneOffset());
  bits.push(navigator.hardwareConcurrency || 0);
  bits.push(navigator.deviceMemory || 0);
  bits.push(navigator.platform || '');

  const joined = bits.join('||');
  let hex;
  if (window.crypto && crypto.subtle && crypto.subtle.digest) {
    try {
      const enc = new TextEncoder();
      const buf = await crypto.subtle.digest('SHA-256', enc.encode(joined));
      hex = Array.from(new Uint8Array(buf)).map(b => b.toString(16).padStart(2, '0')).join('');
    } catch (e) { hex = fnv1aHex(joined); }
  } else {
    hex = fnv1aHex(joined);
  }
  let nonce = localStorage.getItem('rtl-fm-fp-nonce');
  if (!nonce) {
    nonce = (crypto.randomUUID ? crypto.randomUUID() : fnv1aHex(joined + Math.random() + Date.now()));
    localStorage.setItem('rtl-fm-fp-nonce', nonce);
  }
  const fp = hex + '-' + nonce;
  localStorage.setItem('rtl-fm-fp', fp);
  return fp;
}
function fnv1aHex(str) {
  let h1 = 0x811c9dc5 >>> 0;
  let h2 = 0xcbf29ce4 >>> 0;
  for (let i = 0; i < str.length; i++) {
    const c = str.charCodeAt(i);
    h1 = Math.imul((h1 ^ c) >>> 0, 16777619) >>> 0;
    h2 = Math.imul((h2 ^ (c + 0x9e37 + i)) >>> 0, 16777619) >>> 0;
  }
  return h1.toString(16).padStart(8, '0') + h2.toString(16).padStart(8, '0');
}

// Rebuild the dial scale (and re-seat the needle) when the breakpoint changes.
let resizeT = null;
window.addEventListener('resize', () => {
  clearTimeout(resizeT);
  resizeT = setTimeout(() => {
    if (builtBand === dialKey()) return;
    buildDialScale();
    if (!scanning) {
      const nf = currentWinner != null ? currentWinner / 1e6 : (BAND.lo + BAND.hi) / 2;
      updateNeedle(nf, false);
    }
  }, 150);
});

// ---------- boot ----------
buildDialScale();
buildVu();
buildKnob();
updateKnob();
updateNeedle((BAND.lo + BAND.hi) / 2, false);
updatePlayGlyph();

(async () => {
  stripAdminToken();
  applyRole();
  if (!STREAM_PROFILES[streamProfile]) streamProfile = 'low';
  if (isMobile()) streamProfile = 'low'; // mobile can't decode WAV/PCM
  updateKnob();
  try {
    myFingerprint = await computeFingerprint();
  } catch (e) {
    let id = sessionStorage.getItem('rtl-fm-fp-fallback');
    if (!id) {
      id = 'rnd-' + Math.random().toString(36).slice(2) + Date.now().toString(36);
      sessionStorage.setItem('rtl-fm-fp-fallback', id);
    }
    myFingerprint = id;
    console.warn('fingerprint computation failed, using random id:', e);
  }
  loadMeta();
  openSse();
})();
</script>
</body>
</html>
"##;
