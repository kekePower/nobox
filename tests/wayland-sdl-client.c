#include <SDL.h>

int main(void) {
  if (SDL_Init(SDL_INIT_VIDEO) != 0) {
    return 1;
  }
  SDL_Window *window = SDL_CreateWindow(
      "Nobox SDL Wayland Fixture", SDL_WINDOWPOS_CENTERED,
      SDL_WINDOWPOS_CENTERED, 640, 360, SDL_WINDOW_SHOWN | SDL_WINDOW_RESIZABLE);
  if (window == NULL) {
    SDL_Quit();
    return 2;
  }
  SDL_Renderer *renderer = SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE);
  if (renderer == NULL) {
    SDL_DestroyWindow(window);
    SDL_Quit();
    return 3;
  }
  SDL_SetRenderDrawColor(renderer, 36, 92, 140, 255);
  SDL_RenderClear(renderer);
  SDL_RenderPresent(renderer);
  for (;;) {
    SDL_Event event;
    while (SDL_PollEvent(&event) != 0) {
      if (event.type == SDL_QUIT) {
        SDL_DestroyRenderer(renderer);
        SDL_DestroyWindow(window);
        SDL_Quit();
        return 0;
      }
    }
    SDL_Delay(10);
  }
}
