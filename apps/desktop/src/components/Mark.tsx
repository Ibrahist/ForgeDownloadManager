export function Mark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={className} aria-hidden="true">
      <path fill="currentColor" d="M5 21h22v3.2c0 1-.8 1.8-1.8 1.8H6.8C5.8 26 5 25.2 5 24.2V21z" />
      <path fill="currentColor" d="M8.5 21 14 7.5h4.2L14.2 21H8.5z" />
      <rect x="16.5" y="6.2" width="10" height="2.5" rx="0.5" transform="rotate(-32 16.5 6.2)" />
      <rect x="24.2" y="4.2" width="2.6" height="5.4" rx="0.4" transform="rotate(-32 24.2 4.2)" />
    </svg>
  );
}
