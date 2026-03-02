# Todo

## Future refactor options

- **GTK template file refactor**: Extract GTK widget construction in `mod.rs` `build_ui` into
  separate UI template files (e.g. using `gtk::Builder` with `.ui` XML files). This would reduce
  the imperative widget setup code and make the layout easier to visualize and modify.
