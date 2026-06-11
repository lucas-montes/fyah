check: https://github.com/masamasa59/ai-agent-papers


we want some dynamic tools reading to have the latest tools created from other agents
I can't see why we would need something other than agents, an agent is a llm client with a field supervisor, that is the hashmap to children process

the main workflow should be:
the user starts a session. First it will share an idea with the AI agent.
the ai agent will ask questions to define requirements, constrains and expectations.
the ai agent will then create a plan, ask the user to confirm it and keep asking questions if there are ambiguities
once the plan defined the ai agent will start to execute the plan
