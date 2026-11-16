import { useId, useRef, useState, type KeyboardEvent } from "react";
import { filterServiceOptions, serviceMatch, serviceOptionLabel, type ServiceOption } from "./serviceComboboxLogic";

export interface ServiceComboboxProps {
  label: string;
  value: string | null;
  options: ServiceOption[];
  onChange: (value: string | null) => void;
  disabled?: boolean;
  inline?: boolean;
  emptyLabel?: string;
}

export default function ServiceCombobox({ label, value, options, onChange, disabled = false, inline = false, emptyLabel = "Все сервисы" }: ServiceComboboxProps) {
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [focused, setFocused] = useState(false);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(-1);
  const current = options.find((option) => option.value === value) ?? (value === null ? undefined : { value });
  const available = current && !options.some((option) => option.value === current.value) ? [...options, current] : options;
  const visible = filterServiceOptions(available, focused ? query : "");
  const listId = `${id}-options`;
  const optionId = (index: number) => `${id}-option-${index}`;

  function choose(option: ServiceOption) {
    onChange(option.value);
    setQuery("");
    setOpen(false);
    setFocused(false);
    setActiveIndex(-1);
  }

  function clear() {
    onChange(null);
    setQuery("");
    setActiveIndex(-1);
    setOpen(true);
    setFocused(true);
    input.current?.focus();
  }

  function onKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setOpen(true);
      if (!visible.length) return;
      setActiveIndex((index) => event.key === "ArrowDown"
        ? (index + 1) % visible.length
        : index <= 0 ? visible.length - 1 : index - 1);
    } else if (event.key === "Enter" && open) {
      const option = visible[activeIndex] ?? visible[0];
      if (option) {
        event.preventDefault();
        choose(option);
      }
    } else if (event.key === "Escape" && open) {
      event.preventDefault();
      setOpen(false);
      setQuery(current ? serviceOptionLabel(current) : emptyLabel);
      setActiveIndex(-1);
    }
  }

  return (
    <div className={`service-combobox${inline ? " inline" : ""}`}>
      <label htmlFor={id}>{label}</label>
      <div className="service-combobox-control">
        <input
          ref={input}
          id={id}
          role="combobox"
          aria-autocomplete="list"
          aria-haspopup="listbox"
          aria-expanded={open}
          aria-controls={listId}
          aria-activedescendant={open && activeIndex >= 0 ? optionId(activeIndex) : undefined}
          value={focused ? query : current ? serviceOptionLabel(current) : emptyLabel}
          disabled={disabled}
          onFocus={() => {
            setFocused(true);
            setQuery("");
            setActiveIndex(-1);
            setOpen(true);
          }}
          onBlur={() => {
            setOpen(false);
            setFocused(false);
            setQuery("");
            setActiveIndex(-1);
          }}
          onChange={(event) => {
            setQuery(event.target.value);
            setActiveIndex(-1);
            setOpen(true);
          }}
          onKeyDown={onKeyDown}
        />
        {value !== null && !disabled && <button type="button" className="service-combobox-clear" aria-label="Очистить сервис" onClick={clear}>×</button>}
        {open && !disabled && <>
          {visible.length > 0 ? <div className="service-combobox-list" id={listId} role="listbox" aria-label={`${label}: подсказки`}>
            {visible.map((option, index) => {
              const optionLabel = serviceOptionLabel(option);
              const match = serviceMatch(optionLabel, query);
              const secondaryMatch = option.secondary ? serviceMatch(option.secondary, query) : null;
              return <div
                key={`${option.value}/${option.secondary ?? ""}`}
                id={optionId(index)}
                className="service-combobox-option"
                role="option"
                aria-selected={index === activeIndex}
                onMouseDown={(event) => event.preventDefault()}
                onMouseEnter={() => setActiveIndex(index)}
                onClick={() => choose(option)}
              >
                <span>{match ? <>{match.before}<mark>{match.match}</mark>{match.after}</> : optionLabel}</span>
                {option.secondary && <small>{secondaryMatch ? <>{secondaryMatch.before}<mark>{secondaryMatch.match}</mark>{secondaryMatch.after}</> : option.secondary}</small>}
              </div>;
            })}
          </div> : query.trim() && <div className="service-combobox-list service-combobox-empty" role="status">Нет совпадений</div>}
        </>}
      </div>
    </div>
  );
}
