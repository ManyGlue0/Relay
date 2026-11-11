import { motion } from "framer-motion";
import { useCallback } from "react";
import { pageSpring } from "../../lib/motion";
import { useEscape, useStore } from "../../lib/store";
import { About } from "./About";
import { General } from "./General";
import { Models } from "./Models";
import { Presets } from "./Presets";
import { Skills } from "./Skills";
import { Stats } from "./Stats";
import { Tools } from "./Tools";
import { You } from "./You";

/** Settings take the window's pane; the sidebar lists the sections. */
export function SettingsPage() {
  const section = useStore((s) => s.section);
  const editing = useStore((s) => s.editing);
  // Escape leaves an editor first, then Settings.
  const onEscape = useCallback(() => {
    if (useStore.getState().editing) useStore.setState({ editing: null });
    else useStore.getState().closeSettings();
  }, []);
  useEscape(true, onEscape);
  return (
    <div className="settings-pane">
      {/* Each page (and each editor) slides in fresh; nothing waits on an exit. */}
      <motion.div
        key={`${section}:${editing ?? "home"}`}
        className="settings-scroll"
        initial={{ opacity: 0, x: 18 }}
        animate={{ opacity: 1, x: 0 }}
        transition={{ x: pageSpring, opacity: { duration: 0.18 } }}
      >
        {section === "general" && <General />}
        {section === "you" && <You />}
        {section === "models" && <Models />}
        {section === "presets" && <Presets />}
        {section === "tools" && <Tools />}
        {section === "skills" && <Skills />}
        {section === "stats" && <Stats />}
        {section === "about" && <About />}
      </motion.div>
    </div>
  );
}
