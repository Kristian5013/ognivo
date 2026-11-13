// VS Code's own icons (codicons), as the rest of the editor draws them.

export function Icon({ name, spin, className, title }: { name: string; spin?: boolean; className?: string; title?: string }) {
  return (
    <span
      className={`codicon codicon-${name}${spin ? " codicon-modifier-spin" : ""}${className ? ` ${className}` : ""}`}
      aria-hidden={title ? undefined : true}
      title={title}
    />
  );
}

/** A button of one icon, named by its title. */
export function IconButton({
  icon,
  title,
  onClick,
  className,
  disabled,
}: {
  icon: string;
  title: string;
  onClick: () => void;
  className?: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className={`icon-button${className ? ` ${className}` : ""}`}
      title={title}
      aria-label={title}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
    >
      <Icon name={icon} />
    </button>
  );
}
