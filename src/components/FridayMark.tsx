interface FridayMarkProps {
  size?: number;
}

export function FridayMark({ size = 24 }: FridayMarkProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 1024 1024"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      aria-label="Friday"
    >
      <rect width="1024" height="1024" rx="224" fill="#0A0A0A" />
      <rect
        x="2"
        y="2"
        width="1020"
        height="1020"
        rx="222"
        stroke="rgba(255,255,255,0.05)"
        strokeWidth="2"
      />
      <rect x="372" y="280" width="80" height="464" rx="16" fill="#E8E8E8" />
      <rect x="372" y="280" width="280" height="80" rx="16" fill="#E8E8E8" />
      <rect x="372" y="472" width="150" height="72" rx="16" fill="#E8E8E8" />
      <path
        d="M 522 508 L 568 508 L 586 458 L 610 558 L 634 508 L 692 508"
        stroke="#3B82F6"
        strokeWidth="14"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}
