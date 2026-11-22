REVERSE SWELL 1.1.0 for Mac
Reverse reverb that swells into the first hit of a sound
VST3 effect, Apple Silicon and Intel

-------------------------------------------------------------------
INSTALL
-------------------------------------------------------------------
1. Unzip "Reverse Swell.vst3.zip" (double-click it).
2. In Finder press Cmd+Shift+G, paste this and press Enter:
      ~/Library/Audio/Plug-Ins/VST3
3. Drag "Reverse Swell.vst3" into that folder.
4. The plugin isn't from the App Store or a paid Apple developer
   account, so macOS blocks it until you allow it. Open Terminal
   (Cmd+Space, type Terminal) and paste this line, then press Enter:

      xattr -dr com.apple.quarantine ~/Library/Audio/Plug-Ins/VST3/Reverse\ Swell.vst3

5. In FL Studio: Options > Manage plugins > Find plugins.
   It shows up as "Reverse Swell" (vendor: Second Generation).

-------------------------------------------------------------------
RENDER A SWELL TO AUDIO
-------------------------------------------------------------------
1. Put Reverse Swell on the mixer track of the sound.
2. Play the part. The swell from the first hit shows up in the
   LAST SWELL box at the bottom of the plugin.
3. Drag the waveform from that box into the FL playlist.

The file is the swell only, ending exactly at the hit. With SYNC on it
is exactly the chosen length (for example 1/2), so put its end on the
grid line where the hit starts. Each new swell replaces the one in the
box; earlier ones stay saved in Music > Reverse Swell.
"SHOW IN FINDER" opens that folder if you'd rather drag from Finder.

Once the swell is printed you can turn DRY on and WET down, or remove
the plugin, if you don't want it doubled.

-------------------------------------------------------------------
HOW THE LIVE EFFECT WORKS
-------------------------------------------------------------------
The plugin has to look ahead, so it delays the track by the swell
length and tells FL. FL's automatic plugin delay compensation keeps
everything in time. Playing live through it you'll hear that delay.
If timing feels off right after changing LENGTH or the tempo, stop
and restart playback.

-------------------------------------------------------------------
CONTROLS
-------------------------------------------------------------------
Knobs: drag up/down, hold Shift for fine moves, double-click to reset.
The HIT light flashes when a swell gets triggered.

SWELL   SYNC      On: LENGTH follows the project tempo. Off: ms.
        LENGTH    1/16 to 2 bars, or 50 ms to 4 s.
        CURVE     Low = long gradual rise, high = sucks in hard at the end.
        SOURCE    How much of the first hit feeds the reverb (20-500 ms).
TONE    LOW CUT   Keeps lows out of the swell (default 250 Hz).
        HIGH CUT  Tames the top end.
        DARK      Darker start, opening up into the hit.
        WIDTH     0% mono, 100% full stereo.
TRIGGER THRESHOLD Level that counts as a hit.
        GAP       How long it must be quiet before a new swell can
                  trigger, so a phrase only gets one, into its first hit.
OUTPUT  WET       Swell volume (also the volume of the dragged file).
        CUT AT HIT Mutes the swell the instant the hit lands.
        DRY       Off = swell only.

No swell? The sound needs a moment of quiet before it. If the track
never gets quiet (long tails or pads underneath), raise THRESHOLD.
