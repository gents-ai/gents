/* The kit's Tabs and Select are compound parts; a plugin gets one component
   and a list. */
import type { ReactNode } from "react";
import {
  Select as KitSelect,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@gents/ui/components/select";
import {
  Tabs as KitTabs,
  TabsContent,
  TabsList,
  TabsTrigger,
} from "@gents/ui/components/tabs";

export interface TabItem {
  value: string;
  label: ReactNode;
  content: ReactNode;
}

export interface TabsProps {
  items: TabItem[];
  /** the open tab; omit it and the tabs keep their own, starting on the first */
  value?: string;
  onChange?: (value: string) => void;
}

export function Tabs({ items, value, onChange }: TabsProps) {
  return (
    <KitTabs
      data-slot="sdk-tabs"
      value={value}
      defaultValue={value === undefined ? items[0]?.value : undefined}
      onValueChange={onChange && ((next) => onChange(String(next)))}
    >
      <TabsList variant="line">
        {items.map((item) => (
          <TabsTrigger key={item.value} value={item.value}>
            {item.label}
          </TabsTrigger>
        ))}
      </TabsList>
      {items.map((item) => (
        <TabsContent
          key={item.value}
          value={item.value}
          className="flex flex-col gap-3"
        >
          {item.content}
        </TabsContent>
      ))}
    </KitTabs>
  );
}

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectProps {
  /** plain strings, or `{ value, label }` */
  options: Array<string | SelectOption>;
  value?: string;
  onChange?: (value: string) => void;
  /** shown while nothing is chosen */
  placeholder?: string;
  /** names the control for a screen reader */
  label?: string;
}

export function Select({ options, value, onChange, placeholder, label }: SelectProps) {
  const items = options.map((option) =>
    typeof option === "string" ? { value: option, label: option } : option,
  );
  return (
    <KitSelect
      items={items}
      value={value ?? null}
      onValueChange={(next) => {
        if (typeof next === "string") onChange?.(next);
      }}
    >
      <SelectTrigger size="sm" aria-label={label} data-slot="sdk-select">
        <SelectValue placeholder={placeholder} />
      </SelectTrigger>
      <SelectContent alignItemWithTrigger={false}>
        {items.map((item) => (
          <SelectItem key={item.value} value={item.value}>
            {item.label}
          </SelectItem>
        ))}
      </SelectContent>
    </KitSelect>
  );
}
