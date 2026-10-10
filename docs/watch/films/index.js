// The films the player knows. `src` is relative to this file. A film whose module does not exist
// yet sets `available: false` and the player shows `note` instead of trying to load it.
// Unlisted films (`listed: false`) open only by URL (?film=<id>).

export const FILMS = [
  {
    id: 'story',
    label: 'The story',
    kicker: 'A film about Tovek',
    title: 'One function, seventeen releases',
    description: 'The same function, decompiled by every Tovek release since medal.',
    src: './story.js',
    available: true,
    note: 'The story of Tovek is being cut. It opens here soon.',
  },
  {
    id: 'v26',
    label: 'Tovek V2.6',
    kicker: 'Launch film',
    title: 'Tovek V2.6',
    description: 'What Tovek is, and what V2.6 brings.',
    src: './v26.js',
    available: false,
    note: 'Coming with the V2.6 release.',
  },
  {
    id: 'demo',
    label: 'Engine reel',
    kicker: 'Engine reel',
    title: 'Engine reel',
    src: './demo.js',
    available: true,
    listed: false,
  },
];

export const DEFAULT_FILM = 'story';

export const filmById = (id) => FILMS.find((f) => f.id === id) || null;
