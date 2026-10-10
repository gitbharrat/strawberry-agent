// Inline 16px stroke icons.
//
// A whole icon package for two dozen glyphs is not worth the bytes or the
// runtime; these are hand-written paths on a shared 16-unit grid so weight and
// optical size stay consistent.

import type { JSX } from "solid-js";

interface Props extends JSX.SvgSVGAttributes<SVGSVGElement> {
  size?: number;
}

function Icon(props: Props & { children: JSX.Element }) {
  const { size, children, ...rest } = props;
  return (
    <svg
      width={size ?? 16}
      height={size ?? 16}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      stroke-width="1.5"
      stroke-linecap="round"
      stroke-linejoin="round"
      aria-hidden="true"
      {...rest}
    >
      {children}
    </svg>
  );
}

export const Chevron = (p: Props) => (
  <Icon {...p}>
    <path d="M6 3.5 10.5 8 6 12.5" />
  </Icon>
);
export const ChevronDown = (p: Props) => (
  <Icon {...p}>
    <path d="M3.5 6 8 10.5 12.5 6" />
  </Icon>
);
export const Plus = (p: Props) => (
  <Icon {...p}>
    <path d="M8 3.5v9M3.5 8h9" />
  </Icon>
);
/** Two Fusion tiles joined by a pipe — the execution graph. */
export const NodeGraph = (p: Props) => (
  <Icon {...p}>
    <rect x="1.5" y="3" width="5.5" height="3.6" />
    <rect x="9" y="9.4" width="5.5" height="3.6" />
    <path d="M7 4.8h3.2v6.4" />
  </Icon>
);
export const Pin = (p: Props) => (
  <Icon {...p}>
    <path d="M9.5 2 14 6.5l-3 1-2.5 4L4 7l4-2.5Z" />
    <path d="m5.5 10.5-3.5 3.5" />
  </Icon>
);
export const Search = (p: Props) => (
  <Icon {...p}>
    <circle cx="7.2" cy="7.2" r="3.9" />
    <path d="m10.2 10.2 2.4 2.4" />
  </Icon>
);
export const Close = (p: Props) => (
  <Icon {...p}>
    <path d="m4 4 8 8M12 4l-8 8" />
  </Icon>
);
export const Settings = (p: Props) => (
  <Icon {...p}>
    {/* A real cog: eight teeth on a 16-unit grid, not a sun. */}
    <path d="M6.6 2.1h2.8l.3 1.6 1.2.7 1.5-.6 1.4 2.4-1.2 1.1v1.4l1.2 1.1-1.4 2.4-1.5-.6-1.2.7-.3 1.6H6.6l-.3-1.6-1.2-.7-1.5.6-1.4-2.4L3.4 8.7V7.3L2.2 6.2l1.4-2.4 1.5.6 1.2-.7z" />
    <circle cx="8" cy="8" r="2" />
  </Icon>
);
export const Terminal = (p: Props) => (
  <Icon {...p}>
    <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.6" />
    <path d="m4.8 6.4 1.8 1.8-1.8 1.8M8.8 10.2h2.6" />
  </Icon>
);
export const Folder = (p: Props) => (
  <Icon {...p}>
    <path d="M1.8 4.3c0-.7.6-1.3 1.3-1.3h2.5l1.4 1.6h5c.7 0 1.3.6 1.3 1.3v5.8c0 .7-.6 1.3-1.3 1.3H3.1c-.7 0-1.3-.6-1.3-1.3z" />
  </Icon>
);
export const FolderOpen = (p: Props) => (
  <Icon {...p}>
    <path d="M1.8 12V4.3c0-.7.6-1.3 1.3-1.3h2.5l1.4 1.6h5c.7 0 1.3.6 1.3 1.3v.9" />
    <path d="M1.8 12 3.6 7h11l-1.8 5z" />
  </Icon>
);
export const File = (p: Props) => (
  <Icon {...p}>
    <path d="M9 1.8H4.3c-.7 0-1.3.6-1.3 1.3v9.8c0 .7.6 1.3 1.3 1.3h7.4c.7 0 1.3-.6 1.3-1.3V5.8z" />
    <path d="M9 1.8v4h4" />
  </Icon>
);
export const Chat = (p: Props) => (
  <Icon {...p}>
    <path d="M14 9.5c0 .8-.7 1.5-1.5 1.5H6l-3 2.5V4.5C3 3.7 3.7 3 4.5 3h8c.8 0 1.5.7 1.5 1.5z" />
  </Icon>
);
export const Send = (p: Props) => (
  <Icon {...p}>
    <path d="M14 8 2.5 2.8l2 5.2-2 5.2z" />
    <path d="M4.5 8H14" />
  </Icon>
);
export const Stop = (p: Props) => (
  <Icon {...p}>
    <rect x="4" y="4" width="8" height="8" rx="1.4" fill="currentColor" stroke="none" />
  </Icon>
);
export const Attach = (p: Props) => (
  <Icon {...p}>
    <path d="M13 7.3 8 12.3a3.2 3.2 0 0 1-4.5-4.5l5.2-5.2a2.1 2.1 0 1 1 3 3l-5.2 5.2a1 1 0 0 1-1.5-1.5l4.8-4.8" />
  </Icon>
);
export const Copy = (p: Props) => (
  <Icon {...p}>
    <rect x="5.5" y="5.5" width="8" height="8" rx="1.4" />
    <path d="M10.5 5.5v-1c0-.8-.7-1.5-1.5-1.5H4c-.8 0-1.5.7-1.5 1.5V9c0 .8.7 1.5 1.5 1.5h1" />
  </Icon>
);
export const Check = (p: Props) => (
  <Icon {...p}>
    <path d="m3.5 8.5 3 3 6-7" />
  </Icon>
);
export const Refresh = (p: Props) => (
  <Icon {...p}>
    <path d="M13.3 7a5.3 5.3 0 1 0-.5 3.4" />
    <path d="M13.6 3.4V7h-3.5" />
  </Icon>
);
export const Trash = (p: Props) => (
  <Icon {...p}>
    <path d="M2.8 4.3h10.4M6 4.3V3c0-.6.5-1 1-1h2c.5 0 1 .4 1 1v1.3M4.3 4.3l.6 8.4c0 .7.6 1.3 1.3 1.3h3.6c.7 0 1.3-.6 1.3-1.3l.6-8.4" />
  </Icon>
);
export const Edit = (p: Props) => (
  <Icon {...p}>
    <path d="M11.2 2.6a1.7 1.7 0 0 1 2.4 2.4l-7.6 7.6-3.2.8.8-3.2z" />
  </Icon>
);
export const Sparkle = (p: Props) => (
  <Icon {...p}>
    <path d="M8 1.8 9.4 6l4.2 1.4L9.4 8.8 8 13l-1.4-4.2L2.4 7.4 6.6 6z" />
  </Icon>
);
export const Brain = (p: Props) => (
  <Icon {...p}>
    <path d="M6 2.6a2.1 2.1 0 0 0-2.1 2.1A2 2 0 0 0 2.5 6.6c0 .8.4 1.4 1 1.8-.4.4-.6.9-.6 1.5a2 2 0 0 0 2 2c0 .9.8 1.6 1.7 1.6.6 0 1.1-.3 1.4-.8V3.9A1.6 1.6 0 0 0 6 2.6" />
    <path d="M10 2.6a2.1 2.1 0 0 1 2.1 2.1 2 2 0 0 1 1.4 1.9c0 .8-.4 1.4-1 1.8.4.4.6.9.6 1.5a2 2 0 0 1-2 2c0 .9-.8 1.6-1.7 1.6" />
  </Icon>
);
export const Warning = (p: Props) => (
  <Icon {...p}>
    <path d="M7 2.6 1.9 11.5c-.4.7.1 1.6 1 1.6h10.2c.9 0 1.4-.9 1-1.6L9 2.6a1.2 1.2 0 0 0-2 0" />
    <path d="M8 6.2v3M8 11.2h.01" />
  </Icon>
);
export const Info = (p: Props) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="6.2" />
    <path d="M8 7.4v3.4M8 5.2h.01" />
  </Icon>
);
export const Server = (p: Props) => (
  <Icon {...p}>
    <rect x="2" y="2.6" width="12" height="4.6" rx="1.2" />
    <rect x="2" y="8.8" width="12" height="4.6" rx="1.2" />
    <path d="M4.6 4.9h.01M4.6 11.1h.01" />
  </Icon>
);
export const Layers = (p: Props) => (
  <Icon {...p}>
    <path d="M8 1.8 1.8 5 8 8.2 14.2 5z" />
    <path d="m1.8 8.4 6.2 3.2 6.2-3.2" />
    <path d="m1.8 11.4 6.2 3.2 6.2-3.2" />
  </Icon>
);
export const Diff = (p: Props) => (
  <Icon {...p}>
    <path d="M4.2 2.4v11.2M2.2 4.4h4M11.8 13.6V2.4M9.8 11.6h4" />
  </Icon>
);
export const Undo = (p: Props) => (
  <Icon {...p}>
    <path d="M3 7h7.2a3.3 3.3 0 0 1 0 6.6H6.8" />
    <path d="M5.6 4.2 2.8 7l2.8 2.8" />
  </Icon>
);
export const Split = (p: Props) => (
  <Icon {...p}>
    <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.6" />
    <path d="M9 2.8v10.4" />
  </Icon>
);
export const Sidebar = (p: Props) => (
  <Icon {...p}>
    <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.6" />
    <path d="M6.2 2.8v10.4" />
  </Icon>
);
export const Play = (p: Props) => (
  <Icon {...p}>
    <path d="M4.5 3.2 12 8l-7.5 4.8z" />
  </Icon>
);
export const Image = (p: Props) => (
  <Icon {...p}>
    <rect x="1.8" y="2.8" width="12.4" height="10.4" rx="1.6" />
    <circle cx="5.9" cy="6.4" r="1.1" />
    <path d="m2.4 11.6 3.2-3 2.6 2.4 2.4-2.2 3.4 3" />
  </Icon>
);
export const Globe = (p: Props) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="6.2" />
    <path d="M1.9 8h12.2M8 1.8c1.7 1.8 2.6 4 2.6 6.2S9.7 12.4 8 14.2C6.3 12.4 5.4 10.2 5.4 8S6.3 3.6 8 1.8" />
  </Icon>
);
export const Key = (p: Props) => (
  <Icon {...p}>
    <circle cx="10.6" cy="5.4" r="2.9" />
    <path d="M8.5 7.5 2.6 13.4M4.8 11.2l1.5 1.5M6.4 9.6l1.5 1.5" />
  </Icon>
);
export const Command = (p: Props) => (
  <Icon {...p}>
    <path d="M5.5 2.5a1.7 1.7 0 1 0 1.7 1.7v7.6a1.7 1.7 0 1 0 1.7-1.7H4.2a1.7 1.7 0 1 0 1.7 1.7V4.2a1.7 1.7 0 1 0-1.7 1.7h7.6a1.7 1.7 0 1 0-1.7-1.7" />
  </Icon>
);
export const Dot = (p: Props) => (
  <Icon {...p}>
    <circle cx="8" cy="8" r="3" fill="currentColor" stroke="none" />
  </Icon>
);
export const Code = (p: Props) => (
  <Icon {...p}>
    <path d="m5.6 4.4-3.8 3.6 3.8 3.6M10.4 4.4l3.8 3.6-3.8 3.6" />
  </Icon>
);
export const Network = (p: Props) => (
  <Icon {...p}>
    <circle cx="8" cy="3.4" r="1.9" />
    <circle cx="3.4" cy="12" r="1.9" />
    <circle cx="12.6" cy="12" r="1.9" />
    <path d="M7 5.1 4.3 10.3M9 5.1l2.7 5.2M5.3 12h5.4" />
  </Icon>
);
export const Maximize = (p: Props) => (
  <Icon {...p}>
    <path d="M9.4 2.6h4v4M13.4 2.6 9.2 6.8M6.6 13.4h-4v-4M2.6 13.4l4.2-4.2" />
  </Icon>
);
export const Minimize = (p: Props) => (
  <Icon {...p}>
    <path d="M13.6 6.4h-4v-4M9.6 6.4l4-4M2.4 9.6h4v4M6.4 9.6l-4 4" />
  </Icon>
);
export const History = (p: Props) => (
  <Icon {...p}>
    <path d="M2.4 8a5.6 5.6 0 1 1 1.6 3.9M2.2 8.4 2.4 6l2.2 1.2" />
    <path d="M8 5.2V8l2.2 1.4" />
  </Icon>
);
