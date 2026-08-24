---
status: accepted
---

# Make the API authoritative for room lifetime

The API server owns Slopcast room lifetime. It explicitly creates tagged LiveKit rooms, records a private close key for each room, and issues spectator credentials only for registered active rooms. Room closure requires that key and deletes the LiveKit room. The server also deletes tagged rooms on startup and closes a room after its presenter has been absent for 60 seconds. This makes room links revocable without exposing LiveKit administrator credentials or letting anyone close a room by guessing its code.

Rooms do not expire after 24 hours while a presenter remains connected. An API restart deliberately ends Slopcast rooms, and endpoint replacement may briefly prepare a second tagged room before closing the first.
