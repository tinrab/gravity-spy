# Gravity Spy

A 2D platformer game with unique gravity.
You control a spy (the ball) across levels and have to avoid agents.
The gravity of an object works along the direction towards the nearest surface.

# Goal

Your job is to rebuild an old game, "Gravity Spy", which I lost source code for.
I initial developed the game in Java, but you'll use Rust and Bevy game engine.
All that I have left is some assets like screenshots and video in ./old-assets directory.
The source code and the game itself are lost, which is why I want to rebuild it.

The design of the game is minimalistic, as you can see from the screenshots.
One notable things is the 2d lights and their shadows.
The shadows were achieved with raytracing with the physics engine, that allowed for big shadows.
A popular technique is to use shadow maps with shaders to make pixel lights in 2d, but that wouldn't be in the style of this game.
Note that only environment surfaces cast shadows.
