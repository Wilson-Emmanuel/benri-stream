for this role: i got a response:

Software X Discover Engineer
Overview
This exercise evaluates architectural thinking, code quality, and communication skills.
You have 1 week to complete the task.
Goal
Design and implement a minimal private video streaming service. The main focus is on
strong architecture design and maintainable codebase. A functional UI is sufficient.
Core requirements
1. Users can upload video files
   • The upload size is limited to 1GB
   • Support common video formats
   • Videos can be uploaded anonymously
   • The system generates a shareable link per video
2. Users can stream videos by accessing the shareable link via the browser.
3. Time-to-stream (the time from starting a video upload until it can be streamed to users) is
   more important than high video quality.
4. Provide a short document explaining the core architecture decisions and overall design.
   Bonus
1. Playback performance should feel consistent, regardless of file size.
2. The system should be able to scale horizontally.
3. The architecture needs to be cost efficient (cheap to run).
   Guidelines
1. The preferred stack is Rust & Svelte. You’re welcome to introduce additional tools you
   find appropriate.
2. We welcome the use of AI tools, but please don’t collaborate with other people.
3. It’s understandable if you don’t manage to implement everything in code, but completing
   the architecture and system design is a hard requirement.
4. We value thoughtful architectural decisions, and clean code.
5. You do not need to implement authentication, or write IaC.
   Submission
   Once you’ve finished the task, please create a ZIP file containing all of the files you’ve
   prepared and submit it via this form:
   https://forms.gle/6jZCyGmf3UyEBXAZ6