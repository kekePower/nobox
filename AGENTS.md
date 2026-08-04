This is a wild idea that I've been thinking about for a while and I now want to explore the feasibility of doing it and doing it properly.

## The idea

I want to use Openbox, `../openbox`, as the main inspiration to draw almost everything from. Openbox has been my daily driver for many, many years and even though it still works and is still very stable and resource efficient I believe that it can be done better.

That's why I want to create `nobox`.
nobox will be written in Rust and will use Openbox as its main inspiration. The goal is to create a window manager that is more modern, efficient, and user-friendly while maintaining the core principles that make Openbox great.

I would also like to support Wayland natively (even though I've never used it at all).

Openbox is a very old project by now and I also know that it contains a lot of "gotchas" and weird edge-cases over the years that we should really honor and, at least, test.

I also know that Openbox uses XML for most of its configurations. Perhaps it's time to update to something more modern and easier to maintain. I also think that having fewer config files is a good thing(tm).
The `autostart` is awesome and I love its simplicity and want to keep it.

It's also time to update the theming engine and make it more modern and easier to use. I want to make it easy for users to create their own themes and share them with others.

Also, time to make the configuration applications/modals more modern and easier to use. I want to make it easy for users to configure their window manager without having to edit config files manually.

I will be the main tester and user of this project.

Make it easy for me to install, configure, enable and test.
